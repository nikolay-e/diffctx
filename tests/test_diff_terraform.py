import pytest

from treemapper.diffctx import build_diff_context


def _extract_files_from_tree(tree: dict) -> set[str]:
    files = set()
    if tree.get("type") == "diff_context":
        for frag in tree.get("fragments", []):
            path = frag.get("path", "")
            if path:
                files.add(path.split("/")[-1])
        return files

    def traverse(node):
        if node.get("type") == "file":
            files.add(node["name"])
        for child in node.get("children", []):
            traverse(child)

    traverse(tree)
    return files


@pytest.fixture
def terraform_project(git_with_commits):
    return git_with_commits


class TestTerraformLambda:
    def test_tf_441_lambda_function(self, terraform_project):
        terraform_project.add_file(
            "main.py",
            """def handler(event, context):
    return {'statusCode': 200, 'body': 'Hello'}
""",
        )
        terraform_project.add_file(
            "lambda.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"
  role          = aws_iam_role.lambda_exec.arn
  handler       = "index.handler"
  runtime       = "nodejs16.x"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "lambda.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"
  role          = aws_iam_role.lambda_exec.arn
  handler       = "main.handler"
  runtime       = "python3.9"
}
""",
        )
        terraform_project.commit("Update Lambda handler")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "lambda.tf" in selected

    def test_tf_442_lambda_environment(self, terraform_project):
        terraform_project.add_file(
            "app.py",
            """import os
db_host = os.environ.get('DB_HOST')
print(f'Connecting to {db_host}')
""",
        )
        terraform_project.add_file(
            "lambda.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"
  handler       = "app.handler"
  runtime       = "python3.9"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "lambda.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"
  handler       = "app.handler"
  runtime       = "python3.9"

  environment {
    variables = {
      DB_HOST = var.db_host
      DB_NAME = var.db_name
    }
  }
}
""",
        )
        terraform_project.commit("Add Lambda environment")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "lambda.tf" in selected

    def test_tf_443_api_gateway_route(self, terraform_project):
        terraform_project.add_file(
            "handler.py",
            """def get_users(event, context):
    return {'statusCode': 200, 'body': '[]'}
""",
        )
        terraform_project.add_file(
            "api.tf",
            """resource "aws_apigatewayv2_api" "api" {
  name          = "my-api"
  protocol_type = "HTTP"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "api.tf",
            """resource "aws_apigatewayv2_api" "api" {
  name          = "my-api"
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_route" "get_users" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "GET /users"
  target    = "integrations/${aws_apigatewayv2_integration.lambda.id}"
}
""",
        )
        terraform_project.commit("Add API Gateway route")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "api.tf" in selected


class TestTerraformStorage:
    def test_tf_444_s3_bucket(self, terraform_project):
        terraform_project.add_file(
            "app.py",
            """import boto3
s3 = boto3.client('s3')
s3.upload_file('file.txt', 'my-bucket', 'file.txt')
""",
        )
        terraform_project.add_file(
            "storage.tf",
            """# Storage resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "storage.tf",
            """resource "aws_s3_bucket" "data" {
  bucket = "my-data-bucket"

  tags = {
    Name = "Data Bucket"
  }
}

resource "aws_s3_bucket_versioning" "data" {
  bucket = aws_s3_bucket.data.id
  versioning_configuration {
    status = "Enabled"
  }
}
""",
        )
        terraform_project.commit("Add S3 bucket")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "storage.tf" in selected

    def test_tf_445_s3_bucket_policy(self, terraform_project):
        terraform_project.add_file(
            "storage.tf",
            """resource "aws_s3_bucket" "data" {
  bucket = "my-data-bucket"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "storage.tf",
            """resource "aws_s3_bucket" "data" {
  bucket = "my-data-bucket"
}

resource "aws_s3_bucket_policy" "data" {
  bucket = aws_s3_bucket.data.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { AWS = aws_iam_role.lambda_exec.arn }
      Action    = ["s3:GetObject", "s3:PutObject"]
      Resource  = "${aws_s3_bucket.data.arn}/*"
    }]
  })
}
""",
        )
        terraform_project.commit("Add S3 bucket policy")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "storage.tf" in selected

    def test_tf_446_dynamodb_table(self, terraform_project):
        terraform_project.add_file(
            "app.py",
            """import boto3
dynamodb = boto3.resource('dynamodb')
table = dynamodb.Table('users')
table.put_item(Item={'user_id': '123', 'name': 'John'})
""",
        )
        terraform_project.add_file(
            "database.tf",
            """# Database resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "database.tf",
            """resource "aws_dynamodb_table" "users" {
  name           = "users"
  billing_mode   = "PAY_PER_REQUEST"
  hash_key       = "user_id"

  attribute {
    name = "user_id"
    type = "S"
  }

  tags = {
    Name = "Users Table"
  }
}
""",
        )
        terraform_project.commit("Add DynamoDB table")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "database.tf" in selected

    def test_tf_447_rds_instance(self, terraform_project):
        terraform_project.add_file(
            "app.py",
            """import psycopg2
conn = psycopg2.connect(os.environ['DATABASE_URL'])
""",
        )
        terraform_project.add_file(
            "database.tf",
            """# RDS resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "database.tf",
            """resource "aws_db_instance" "main" {
  identifier           = "main-db"
  engine               = "postgres"
  engine_version       = "13.7"
  instance_class       = "db.t3.micro"
  allocated_storage    = 20
  username             = var.db_username
  password             = var.db_password
  skip_final_snapshot  = true
}
""",
        )
        terraform_project.commit("Add RDS instance")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "database.tf" in selected


class TestTerraformSecurity:
    def test_tf_448_security_group_rule(self, terraform_project):
        terraform_project.add_file(
            "security.tf",
            """resource "aws_security_group" "web" {
  name = "web-sg"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "security.tf",
            """resource "aws_security_group" "web" {
  name = "web-sg"

  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
""",
        )
        terraform_project.commit("Add security group rules")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "security.tf" in selected

    def test_tf_449_iam_role(self, terraform_project):
        terraform_project.add_file(
            "iam.tf",
            """# IAM resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "iam.tf",
            """resource "aws_iam_role" "lambda_exec" {
  name = "lambda-exec-role"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}
""",
        )
        terraform_project.commit("Add IAM role")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "iam.tf" in selected

    def test_tf_450_iam_policy(self, terraform_project):
        terraform_project.add_file(
            "iam.tf",
            """resource "aws_iam_role" "lambda_exec" {
  name = "lambda-exec-role"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "iam.tf",
            """resource "aws_iam_role" "lambda_exec" {
  name = "lambda-exec-role"
}

resource "aws_iam_policy" "s3_access" {
  name = "s3-access-policy"

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = ["s3:GetObject", "s3:PutObject"]
      Resource = "arn:aws:s3:::my-bucket/*"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "s3_access" {
  role       = aws_iam_role.lambda_exec.name
  policy_arn = aws_iam_policy.s3_access.arn
}
""",
        )
        terraform_project.commit("Add IAM policy")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "iam.tf" in selected


class TestTerraformCompute:
    def test_tf_451_ec2_instance(self, terraform_project):
        terraform_project.add_file(
            "user_data.sh",
            """#!/bin/bash
yum update -y
yum install -y docker
systemctl start docker
""",
        )
        terraform_project.add_file(
            "compute.tf",
            """# Compute resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "compute.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"

  user_data = file("user_data.sh")

  tags = {
    Name = "Web Server"
  }
}
""",
        )
        terraform_project.commit("Add EC2 instance")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "compute.tf" in selected

    def test_tf_452_autoscaling_group(self, terraform_project):
        terraform_project.add_file(
            "compute.tf",
            """resource "aws_launch_template" "web" {
  name = "web-template"
  image_id = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "compute.tf",
            """resource "aws_launch_template" "web" {
  name = "web-template"
  image_id = var.ami_id
  instance_type = "t3.micro"
}

resource "aws_autoscaling_group" "web" {
  name                = "web-asg"
  min_size            = 1
  max_size            = 10
  desired_capacity    = 2
  vpc_zone_identifier = var.subnet_ids

  launch_template {
    id      = aws_launch_template.web.id
    version = "$Latest"
  }
}
""",
        )
        terraform_project.commit("Add Auto Scaling Group")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "compute.tf" in selected

    def test_tf_453_ecs_service(self, terraform_project):
        terraform_project.add_file(
            "ecs.tf",
            """resource "aws_ecs_cluster" "main" {
  name = "main-cluster"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "ecs.tf",
            """resource "aws_ecs_cluster" "main" {
  name = "main-cluster"
}

resource "aws_ecs_service" "app" {
  name            = "app-service"
  cluster         = aws_ecs_cluster.main.id
  task_definition = aws_ecs_task_definition.app.arn
  desired_count   = 2
  launch_type     = "FARGATE"

  network_configuration {
    subnets         = var.subnet_ids
    security_groups = [aws_security_group.app.id]
  }
}
""",
        )
        terraform_project.commit("Add ECS service")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "ecs.tf" in selected

    def test_tf_454_ecs_task_definition(self, terraform_project):
        terraform_project.add_file(
            "ecs.tf",
            """resource "aws_ecs_cluster" "main" {
  name = "main-cluster"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "ecs.tf",
            """resource "aws_ecs_cluster" "main" {
  name = "main-cluster"
}

resource "aws_ecs_task_definition" "app" {
  family                   = "app-task"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = "256"
  memory                   = "512"

  container_definitions = jsonencode([{
    name  = "app"
    image = "myapp:latest"
    portMappings = [{
      containerPort = 8080
    }]
    environment = [{
      name  = "NODE_ENV"
      value = "production"
    }]
  }])
}
""",
        )
        terraform_project.commit("Add ECS task definition")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "ecs.tf" in selected


class TestTerraformMonitoring:
    def test_tf_455_cloudwatch_alarm(self, terraform_project):
        terraform_project.add_file(
            "monitoring.tf",
            """# Monitoring resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "monitoring.tf",
            """resource "aws_cloudwatch_metric_alarm" "cpu_high" {
  alarm_name          = "cpu-high"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "CPUUtilization"
  namespace           = "AWS/EC2"
  period              = 300
  statistic           = "Average"
  threshold           = 80

  dimensions = {
    AutoScalingGroupName = aws_autoscaling_group.web.name
  }

  alarm_actions = [aws_sns_topic.alerts.arn]
}
""",
        )
        terraform_project.commit("Add CloudWatch alarm")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "monitoring.tf" in selected

    def test_tf_456_sns_topic(self, terraform_project):
        terraform_project.add_file(
            "monitoring.tf",
            """# SNS resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "monitoring.tf",
            """resource "aws_sns_topic" "alerts" {
  name = "alerts-topic"
}

resource "aws_sns_topic_subscription" "email" {
  topic_arn = aws_sns_topic.alerts.arn
  protocol  = "email"
  endpoint  = var.alert_email
}
""",
        )
        terraform_project.commit("Add SNS topic")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "monitoring.tf" in selected

    def test_tf_457_sqs_queue(self, terraform_project):
        terraform_project.add_file(
            "worker.py",
            """import boto3
sqs = boto3.client('sqs')
messages = sqs.receive_message(QueueUrl=queue_url)
""",
        )
        terraform_project.add_file(
            "messaging.tf",
            """# Messaging resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "messaging.tf",
            """resource "aws_sqs_queue" "tasks" {
  name                      = "tasks-queue"
  delay_seconds             = 0
  max_message_size          = 262144
  message_retention_seconds = 345600
  visibility_timeout_seconds = 30

  redrive_policy = jsonencode({
    deadLetterTargetArn = aws_sqs_queue.tasks_dlq.arn
    maxReceiveCount     = 3
  })
}

resource "aws_sqs_queue" "tasks_dlq" {
  name = "tasks-queue-dlq"
}
""",
        )
        terraform_project.commit("Add SQS queue")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "messaging.tf" in selected

    def test_tf_458_eventbridge_rule(self, terraform_project):
        terraform_project.add_file(
            "events.tf",
            """# EventBridge resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "events.tf",
            """resource "aws_cloudwatch_event_rule" "schedule" {
  name                = "daily-cleanup"
  schedule_expression = "cron(0 2 * * ? *)"
}

resource "aws_cloudwatch_event_target" "lambda" {
  rule      = aws_cloudwatch_event_rule.schedule.name
  target_id = "cleanup-lambda"
  arn       = aws_lambda_function.cleanup.arn
}

resource "aws_lambda_permission" "eventbridge" {
  statement_id  = "AllowEventBridge"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.cleanup.function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.schedule.arn
}
""",
        )
        terraform_project.commit("Add EventBridge rule")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "events.tf" in selected


class TestTerraformNetworking:
    def test_tf_459_vpc(self, terraform_project):
        terraform_project.add_file(
            "network.tf",
            """# Network resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block           = "10.0.0.0/16"
  enable_dns_hostnames = true
  enable_dns_support   = true

  tags = {
    Name = "main-vpc"
  }
}
""",
        )
        terraform_project.commit("Add VPC")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "network.tf" in selected

    def test_tf_460_subnet(self, terraform_project):
        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "private" {
  count             = 2
  vpc_id            = aws_vpc.main.id
  cidr_block        = cidrsubnet(aws_vpc.main.cidr_block, 8, count.index)
  availability_zone = data.aws_availability_zones.available.names[count.index]

  tags = {
    Name = "private-subnet-${count.index}"
  }
}
""",
        )
        terraform_project.commit("Add subnet")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "network.tf" in selected

    def test_tf_461_route_table(self, terraform_project):
        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "private" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.1.0/24"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "private" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.1.0/24"
}

resource "aws_route_table" "private" {
  vpc_id = aws_vpc.main.id

  route {
    cidr_block     = "0.0.0.0/0"
    nat_gateway_id = aws_nat_gateway.main.id
  }
}

resource "aws_route_table_association" "private" {
  subnet_id      = aws_subnet.private.id
  route_table_id = aws_route_table.private.id
}
""",
        )
        terraform_project.commit("Add route table")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "network.tf" in selected

    def test_tf_462_nat_gateway(self, terraform_project):
        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "public" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.0.0/24"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "network.tf",
            """resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "public" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.0.0/24"
}

resource "aws_eip" "nat" {
  domain = "vpc"
}

resource "aws_nat_gateway" "main" {
  allocation_id = aws_eip.nat.id
  subnet_id     = aws_subnet.public.id

  depends_on = [aws_internet_gateway.main]
}
""",
        )
        terraform_project.commit("Add NAT gateway")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "network.tf" in selected

    def test_tf_463_alb(self, terraform_project):
        terraform_project.add_file(
            "lb.tf",
            """# Load balancer resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "lb.tf",
            """resource "aws_lb" "main" {
  name               = "main-alb"
  internal           = false
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = var.public_subnet_ids

  enable_deletion_protection = true
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS-1-2-2017-01"
  certificate_arn   = aws_acm_certificate.main.arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app.arn
  }
}
""",
        )
        terraform_project.commit("Add ALB")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "lb.tf" in selected

    def test_tf_464_target_group(self, terraform_project):
        terraform_project.add_file(
            "app.py",
            """from flask import Flask
app = Flask(__name__)

@app.route('/health')
def health():
    return 'OK', 200
""",
        )
        terraform_project.add_file(
            "lb.tf",
            """resource "aws_lb" "main" {
  name = "main-alb"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "lb.tf",
            """resource "aws_lb" "main" {
  name = "main-alb"
}

resource "aws_lb_target_group" "app" {
  name        = "app-tg"
  port        = 8080
  protocol    = "HTTP"
  vpc_id      = aws_vpc.main.id
  target_type = "ip"

  health_check {
    path                = "/health"
    healthy_threshold   = 2
    unhealthy_threshold = 10
    timeout             = 5
    interval            = 30
  }
}
""",
        )
        terraform_project.commit("Add target group")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "lb.tf" in selected

    def test_tf_465_acm_certificate(self, terraform_project):
        terraform_project.add_file(
            "dns.tf",
            """# DNS resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "dns.tf",
            """resource "aws_acm_certificate" "main" {
  domain_name       = "api.example.com"
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_acm_certificate_validation" "main" {
  certificate_arn         = aws_acm_certificate.main.arn
  validation_record_fqdns = [aws_route53_record.cert_validation.fqdn]
}
""",
        )
        terraform_project.commit("Add ACM certificate")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "dns.tf" in selected

    def test_tf_466_route53_record(self, terraform_project):
        terraform_project.add_file(
            "dns.tf",
            """resource "aws_route53_zone" "main" {
  name = "example.com"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "dns.tf",
            """resource "aws_route53_zone" "main" {
  name = "example.com"
}

resource "aws_route53_record" "api" {
  zone_id = aws_route53_zone.main.zone_id
  name    = "api.example.com"
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = true
  }
}
""",
        )
        terraform_project.commit("Add Route53 record")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "dns.tf" in selected

    def test_tf_467_cloudfront(self, terraform_project):
        terraform_project.add_file(
            "cdn.tf",
            """# CDN resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "cdn.tf",
            """resource "aws_cloudfront_distribution" "main" {
  enabled             = true
  is_ipv6_enabled     = true
  default_root_object = "index.html"

  origin {
    domain_name = aws_s3_bucket.website.bucket_regional_domain_name
    origin_id   = "S3-Website"

    s3_origin_config {
      origin_access_identity = aws_cloudfront_origin_access_identity.main.cloudfront_access_identity_path
    }
  }

  default_cache_behavior {
    allowed_methods  = ["GET", "HEAD"]
    cached_methods   = ["GET", "HEAD"]
    target_origin_id = "S3-Website"

    forwarded_values {
      query_string = false
      cookies {
        forward = "none"
      }
    }

    viewer_protocol_policy = "redirect-to-https"
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn = aws_acm_certificate.main.arn
    ssl_support_method  = "sni-only"
  }
}
""",
        )
        terraform_project.commit("Add CloudFront distribution")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "cdn.tf" in selected


class TestTerraformModules:
    def test_tf_468_module_usage(self, terraform_project):
        terraform_project.add_file(
            "modules/vpc/main.tf",
            """resource "aws_vpc" "main" {
  cidr_block = var.cidr_block
}

output "vpc_id" {
  value = aws_vpc.main.id
}
""",
        )
        terraform_project.add_file(
            "modules/vpc/variables.tf",
            """variable "cidr_block" {
  type = string
}
""",
        )
        terraform_project.add_file(
            "main.tf",
            """# Main configuration
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """module "vpc" {
  source     = "./modules/vpc"
  cidr_block = "10.0.0.0/16"
}

resource "aws_subnet" "main" {
  vpc_id     = module.vpc.vpc_id
  cidr_block = "10.0.1.0/24"
}
""",
        )
        terraform_project.commit("Add module usage")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_469_module_variables(self, terraform_project):
        terraform_project.add_file(
            "modules/app/main.tf",
            """resource "aws_lambda_function" "app" {
  function_name = var.name
  runtime       = var.runtime
}
""",
        )
        terraform_project.add_file(
            "modules/app/variables.tf",
            """variable "name" {
  type = string
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "modules/app/variables.tf",
            """variable "name" {
  type = string
}

variable "runtime" {
  type    = string
  default = "python3.9"
}

variable "memory_size" {
  type    = number
  default = 128
}
""",
        )
        terraform_project.commit("Add module variables")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "variables.tf" in selected

    def test_tf_470_module_outputs(self, terraform_project):
        terraform_project.add_file(
            "modules/rds/main.tf",
            """resource "aws_db_instance" "main" {
  identifier = var.identifier
  engine     = "postgres"
}
""",
        )
        terraform_project.add_file(
            "modules/rds/outputs.tf",
            """output "endpoint" {
  value = aws_db_instance.main.endpoint
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "modules/rds/outputs.tf",
            """output "endpoint" {
  value = aws_db_instance.main.endpoint
}

output "port" {
  value = aws_db_instance.main.port
}

output "connection_string" {
  value     = "postgres://${var.username}:${var.password}@${aws_db_instance.main.endpoint}/${var.database}"
  sensitive = true
}
""",
        )
        terraform_project.commit("Add module outputs")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "outputs.tf" in selected


class TestTerraformState:
    def test_tf_471_remote_state(self, terraform_project):
        terraform_project.add_file(
            "backend.tf",
            """terraform {
  backend "local" {}
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "backend.tf",
            """terraform {
  backend "s3" {
    bucket         = "my-terraform-state"
    key            = "prod/terraform.tfstate"
    region         = "us-east-1"
    encrypt        = true
    dynamodb_table = "terraform-locks"
  }
}
""",
        )
        terraform_project.commit("Add remote state")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "backend.tf" in selected

    def test_tf_472_data_source(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = "ami-12345"
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """data "aws_ami" "amazon_linux" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["amzn2-ami-hvm-*-x86_64-gp2"]
  }
}

resource "aws_instance" "web" {
  ami           = data.aws_ami.amazon_linux.id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Add data source")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_473_locals(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  tags = {
    Name = "web-server"
    Environment = "prod"
  }
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """locals {
  common_tags = {
    Project     = "myapp"
    Environment = var.environment
    ManagedBy   = "terraform"
  }
}

resource "aws_instance" "web" {
  tags = merge(local.common_tags, {
    Name = "web-server"
  })
}
""",
        )
        terraform_project.commit("Add locals")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_474_for_each(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_subnet" "main" {
  vpc_id     = aws_vpc.main.id
  cidr_block = "10.0.1.0/24"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """variable "subnets" {
  default = {
    "public-1"  = "10.0.1.0/24"
    "public-2"  = "10.0.2.0/24"
    "private-1" = "10.0.10.0/24"
    "private-2" = "10.0.11.0/24"
  }
}

resource "aws_subnet" "main" {
  for_each   = var.subnets
  vpc_id     = aws_vpc.main.id
  cidr_block = each.value

  tags = {
    Name = each.key
  }
}
""",
        )
        terraform_project.commit("Add for_each")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_475_count(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """variable "instance_count" {
  default = 3
}

resource "aws_instance" "web" {
  count         = var.instance_count
  ami           = var.ami_id
  instance_type = "t3.micro"

  tags = {
    Name = "web-${count.index}"
  }
}
""",
        )
        terraform_project.commit("Add count")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected


class TestTerraformAdvanced:
    def test_tf_476_dynamic_block(self, terraform_project):
        terraform_project.add_file(
            "security.tf",
            """resource "aws_security_group" "web" {
  name = "web-sg"

  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "security.tf",
            """variable "allowed_ports" {
  default = [80, 443, 8080]
}

resource "aws_security_group" "web" {
  name = "web-sg"

  dynamic "ingress" {
    for_each = var.allowed_ports
    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
    }
  }
}
""",
        )
        terraform_project.commit("Add dynamic block")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "security.tf" in selected

    def test_tf_477_provisioner(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"

  provisioner "remote-exec" {
    inline = [
      "sudo yum update -y",
      "sudo yum install -y docker",
      "sudo systemctl start docker"
    ]

    connection {
      type        = "ssh"
      user        = "ec2-user"
      private_key = file(var.private_key_path)
      host        = self.public_ip
    }
  }
}
""",
        )
        terraform_project.commit("Add provisioner")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_478_lifecycle(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"

  lifecycle {
    create_before_destroy = true
    prevent_destroy       = true
    ignore_changes        = [tags]
  }
}
""",
        )
        terraform_project.commit("Add lifecycle")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_479_depends_on(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_ecs_service" "app" {
  name = "app-service"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_ecs_service" "app" {
  name = "app-service"

  depends_on = [
    aws_lb_listener.https,
    aws_iam_role_policy_attachment.ecs_task
  ]
}
""",
        )
        terraform_project.commit("Add depends_on")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_480_terraform_functions(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """locals {
  name = "myapp"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """locals {
  name = "myapp"

  # String functions
  upper_name = upper(local.name)
  trimmed    = trimspace("  hello  ")

  # Collection functions
  merged_tags = merge(var.common_tags, var.extra_tags)
  flattened   = flatten([var.subnet_ids, var.extra_subnet_ids])

  # Encoding functions
  encoded = base64encode("secret")
  decoded = jsondecode(file("config.json"))

  # Math functions
  max_count = max(var.min_count, var.desired_count)
}
""",
        )
        terraform_project.commit("Add Terraform functions")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected


class TestTerraformVariablesModules:
    def test_tf_481_dynamic_block_nested(self, terraform_project):
        terraform_project.add_file(
            "lb.tf",
            """resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "lb.tf",
            """variable "redirects" {
  default = [
    { host = "old.example.com", target = "new.example.com" },
    { host = "legacy.example.com", target = "app.example.com" }
  ]
}

resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"

  dynamic "default_action" {
    for_each = var.redirects
    content {
      type = "redirect"
      redirect {
        host        = default_action.value.target
        port        = "443"
        protocol    = "HTTPS"
        status_code = "HTTP_301"
      }
    }
  }
}
""",
        )
        terraform_project.commit("Add nested dynamic block")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "lb.tf" in selected

    def test_tf_482_data_source_external(self, terraform_project):
        terraform_project.add_file(
            "scripts/get_ami.sh",
            """#!/bin/bash
echo '{"ami_id": "ami-12345678"}'
""",
        )
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami = "ami-static"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """data "external" "ami" {
  program = ["${path.module}/scripts/get_ami.sh"]
}

resource "aws_instance" "web" {
  ami = data.external.ami.result.ami_id
}
""",
        )
        terraform_project.commit("Add external data source")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_483_remote_state_data(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  subnet_id = "subnet-12345"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """data "terraform_remote_state" "vpc" {
  backend = "s3"
  config = {
    bucket = "terraform-state"
    key    = "vpc/terraform.tfstate"
    region = "us-east-1"
  }
}

resource "aws_instance" "web" {
  subnet_id = data.terraform_remote_state.vpc.outputs.subnet_id
}
""",
        )
        terraform_project.commit("Add remote state data source")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_484_provider_config(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami = "ami-12345"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "providers.tf",
            """provider "aws" {
  region = "us-east-1"

  default_tags {
    tags = {
      Environment = "production"
      ManagedBy   = "terraform"
    }
  }

  assume_role {
    role_arn = "arn:aws:iam::123456789:role/TerraformRole"
  }
}
""",
        )
        terraform_project.commit("Add provider config")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "providers.tf" in selected

    def test_tf_485_provider_alias(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_s3_bucket" "main" {
  bucket = "my-bucket"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "providers.tf",
            """provider "aws" {
  region = "us-east-1"
}

provider "aws" {
  alias  = "west"
  region = "us-west-2"
}
""",
        )
        terraform_project.add_file(
            "main.tf",
            """resource "aws_s3_bucket" "main" {
  bucket = "my-bucket"
}

resource "aws_s3_bucket" "replica" {
  provider = aws.west
  bucket   = "my-bucket-replica"
}

resource "aws_s3_bucket_replication_configuration" "main" {
  bucket = aws_s3_bucket.main.id
  role   = aws_iam_role.replication.arn

  rule {
    destination {
      bucket = aws_s3_bucket.replica.arn
    }
  }
}
""",
        )
        terraform_project.commit("Add provider alias for multi-region")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected or "providers.tf" in selected

    def test_tf_486_backend_config(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """terraform {
}

resource "aws_instance" "web" {
  ami = "ami-12345"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """terraform {
  backend "s3" {
    bucket         = "my-terraform-state"
    key            = "prod/terraform.tfstate"
    region         = "us-east-1"
    encrypt        = true
    dynamodb_table = "terraform-locks"
    kms_key_id     = "alias/terraform-state"
  }
}

resource "aws_instance" "web" {
  ami = "ami-12345"
}
""",
        )
        terraform_project.commit("Add backend config")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_487_terraform_version(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami = "ami-12345"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "versions.tf",
            """terraform {
  required_version = ">= 1.5.0, < 2.0.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}
""",
        )
        terraform_project.commit("Add Terraform version constraint")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "versions.tf" in selected

    def test_tf_488_provider_version(self, terraform_project):
        terraform_project.add_file(
            "versions.tf",
            """terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 4.0"
    }
  }
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "versions.tf",
            """terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.5"
    }
    null = {
      source  = "hashicorp/null"
      version = "~> 3.2"
    }
  }
}
""",
        )
        terraform_project.commit("Update provider versions")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "versions.tf" in selected

    def test_tf_489_lifecycle_ignore(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"

  lifecycle {
    ignore_changes = [
      ami,
      user_data,
      tags["LastModified"],
    ]
  }
}
""",
        )
        terraform_project.commit("Add lifecycle ignore_changes")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_490_lifecycle_prevent_destroy(self, terraform_project):
        terraform_project.add_file(
            "database.tf",
            """resource "aws_db_instance" "main" {
  identifier     = "production-db"
  engine         = "postgres"
  instance_class = "db.r5.large"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "database.tf",
            """resource "aws_db_instance" "main" {
  identifier     = "production-db"
  engine         = "postgres"
  instance_class = "db.r5.large"

  lifecycle {
    prevent_destroy = true
  }
}
""",
        )
        terraform_project.commit("Add prevent_destroy")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "database.tf" in selected

    def test_tf_491_depends_on_explicit(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_lambda_function" "api" {
  function_name = "api-handler"

  depends_on = [
    aws_iam_role_policy_attachment.lambda_logs,
    aws_cloudwatch_log_group.lambda,
    aws_vpc_endpoint.s3,
  ]
}
""",
        )
        terraform_project.commit("Add explicit depends_on")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_492_provisioner_local_exec(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"

  provisioner "local-exec" {
    command = "echo ${self.private_ip} >> private_ips.txt"
  }

  provisioner "local-exec" {
    when    = destroy
    command = "sed -i '/${self.private_ip}/d' private_ips.txt"
  }
}
""",
        )
        terraform_project.commit("Add local-exec provisioner")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_493_null_resource(self, terraform_project):
        terraform_project.add_file(
            "scripts/deploy.sh",
            """#!/bin/bash
echo "Deploying application..."
""",
        )
        terraform_project.add_file(
            "main.tf",
            """# Main resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """resource "null_resource" "deploy" {
  triggers = {
    build_version = var.build_version
    config_hash   = md5(file("config.json"))
  }

  provisioner "local-exec" {
    command = "./scripts/deploy.sh ${var.environment}"
  }

  depends_on = [aws_instance.web]
}
""",
        )
        terraform_project.commit("Add null_resource for deployment")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_494_moved_block(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "compute.tf",
            """resource "aws_instance" "web_server" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}

moved {
  from = aws_instance.web
  to   = aws_instance.web_server
}
""",
        )
        terraform_project.add_file(
            "main.tf",
            """# Resources moved to compute.tf
""",
        )
        terraform_project.commit("Refactor with moved block")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "compute.tf" in selected

    def test_tf_495_import_block(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """# Managed resources
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """import {
  to = aws_s3_bucket.existing
  id = "my-existing-bucket"
}

resource "aws_s3_bucket" "existing" {
  bucket = "my-existing-bucket"

  tags = {
    ManagedBy = "terraform"
  }
}
""",
        )
        terraform_project.commit("Add import block for existing resource")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_496_workspace(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = "t3.micro"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """locals {
  environment = terraform.workspace
  is_prod     = terraform.workspace == "production"
}

resource "aws_instance" "web" {
  ami           = var.ami_id
  instance_type = local.is_prod ? "t3.large" : "t3.micro"

  tags = {
    Environment = local.environment
  }
}
""",
        )
        terraform_project.commit("Add workspace-based configuration")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_497_sensitive_variable(self, terraform_project):
        terraform_project.add_file(
            "variables.tf",
            """variable "db_password" {
  type = string
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "variables.tf",
            """variable "db_password" {
  type      = string
  sensitive = true
}

variable "api_key" {
  type      = string
  sensitive = true
}

variable "ssl_certificate" {
  type      = string
  sensitive = true
}
""",
        )
        terraform_project.commit("Mark variables as sensitive")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "variables.tf" in selected

    def test_tf_498_terraform_cloud(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """terraform {
  backend "local" {}
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main.tf",
            """terraform {
  cloud {
    organization = "my-org"

    workspaces {
      tags = ["app:web", "env:production"]
    }
  }
}
""",
        )
        terraform_project.commit("Migrate to Terraform Cloud")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main.tf" in selected

    def test_tf_499_state_encryption(self, terraform_project):
        terraform_project.add_file(
            "backend.tf",
            """terraform {
  backend "s3" {
    bucket = "my-terraform-state"
    key    = "terraform.tfstate"
    region = "us-east-1"
  }
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "backend.tf",
            """terraform {
  backend "s3" {
    bucket         = "my-terraform-state"
    key            = "terraform.tfstate"
    region         = "us-east-1"
    encrypt        = true
    kms_key_id     = "arn:aws:kms:us-east-1:123456789:key/abc123"
    dynamodb_table = "terraform-state-lock"
  }
}
""",
        )
        terraform_project.commit("Add state encryption")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "backend.tf" in selected

    def test_tf_500_override_files(self, terraform_project):
        terraform_project.add_file(
            "main.tf",
            """resource "aws_instance" "web" {
  ami           = "ami-production"
  instance_type = "t3.large"
}
""",
        )
        terraform_project.commit("Initial")

        terraform_project.add_file(
            "main_override.tf",
            """resource "aws_instance" "web" {
  ami           = "ami-development"
  instance_type = "t3.micro"

  tags = {
    Environment = "dev"
  }
}
""",
        )
        terraform_project.commit("Add override file for development")

        tree = build_diff_context(
            root_dir=terraform_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "main_override.tf" in selected
