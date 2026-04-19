GARBAGE_FILES = {
    "unrelated_dir/garbage_utils.py": """def completely_unrelated_garbage_function():
    return "garbage_marker_12345"

def another_unused_helper():
    return "unused_marker_67890"

class UnusedGarbageClass:
    def useless_method(self):
        return "class_garbage_marker"

def garbage_helper_alpha():
    return "garbage_helper_alpha_value"

def garbage_helper_beta():
    return "garbage_helper_beta_value"

def garbage_helper_gamma():
    return "garbage_helper_gamma_value"

def garbage_helper_delta():
    return "garbage_helper_delta_value"

def garbage_helper_epsilon():
    return "garbage_helper_epsilon_value"
""",
    "unrelated_dir/garbage_constants.py": """GARBAGE_CONSTANT_ALPHA = "garbage_alpha_constant"
GARBAGE_CONSTANT_BETA = "garbage_beta_constant"
UNUSED_CONFIG_VALUE = 99999
GARBAGE_CONST_GAMMA = "garbage_gamma_constant"
GARBAGE_CONST_DELTA = "garbage_delta_constant"
GARBAGE_CONST_EPSILON = "garbage_epsilon_constant"
GARBAGE_CONST_ZETA = "garbage_zeta_constant"
GARBAGE_CONST_ETA = "garbage_eta_constant"
GARBAGE_CONST_THETA = "garbage_theta_constant"
GARBAGE_CONST_IOTA = "garbage_iota_constant"
GARBAGE_CONST_KAPPA = "garbage_kappa_constant"
""",
    "unrelated_dir/garbage_module.js": """export function unusedJsGarbage() {
    return "js_garbage_marker_abc";
}

export const GARBAGE_JS_CONST = "js_const_garbage";

export function garbageJsHelperAlpha() {
    return "js_garbage_alpha_marker";
}

export function garbageJsHelperBeta() {
    return "js_garbage_beta_marker";
}

export function garbageJsHelperGamma() {
    return "js_garbage_gamma_marker";
}

export const GARBAGE_JS_CONFIG = {
    unusedKey1: "garbage_config_value_1",
    unusedKey2: "garbage_config_value_2",
    unusedKey3: "garbage_config_value_3",
};
""",
    "unrelated_dir/garbage_types.py": """class GarbageTypeAlpha:
    def garbage_method_one(self):
        return "garbage_type_alpha_one"

    def garbage_method_two(self):
        return "garbage_type_alpha_two"

    def garbage_method_three(self):
        return "garbage_type_alpha_three"

class GarbageTypeBeta:
    def garbage_method_one(self):
        return "garbage_type_beta_one"

    def garbage_method_two(self):
        return "garbage_type_beta_two"

class GarbageTypeGamma:
    def garbage_method_one(self):
        return "garbage_type_gamma_one"

    def garbage_method_two(self):
        return "garbage_type_gamma_two"

    def garbage_method_three(self):
        return "garbage_type_gamma_three"

    def garbage_method_four(self):
        return "garbage_type_gamma_four"
""",
    "unrelated_dir/garbage_services.py": """class GarbageServiceAlpha:
    def process_garbage_alpha(self, data):
        return f"garbage_service_alpha_{data}"

    def validate_garbage_alpha(self, item):
        return "garbage_validation_alpha"

    def transform_garbage_alpha(self, value):
        return "garbage_transform_alpha"

class GarbageServiceBeta:
    def process_garbage_beta(self, data):
        return f"garbage_service_beta_{data}"

    def validate_garbage_beta(self, item):
        return "garbage_validation_beta"

class GarbageServiceGamma:
    def process_garbage_gamma(self, data):
        return f"garbage_service_gamma_{data}"

    def execute_garbage_gamma(self, cmd):
        return "garbage_execution_gamma"

def standalone_garbage_function_one():
    return "standalone_garbage_one"

def standalone_garbage_function_two():
    return "standalone_garbage_two"

def standalone_garbage_function_three():
    return "standalone_garbage_three"
""",
    "unrelated_dir/garbage_handlers.py": """class GarbageHandlerAlpha:
    def handle_garbage_event_alpha(self, event):
        return "garbage_event_alpha_handled"

    def process_garbage_request_alpha(self, request):
        return "garbage_request_alpha_processed"

class GarbageHandlerBeta:
    def handle_garbage_event_beta(self, event):
        return "garbage_event_beta_handled"

    def process_garbage_request_beta(self, request):
        return "garbage_request_beta_processed"

class GarbageHandlerGamma:
    def handle_garbage_event_gamma(self, event):
        return "garbage_event_gamma_handled"

def garbage_event_dispatcher(event_type):
    return f"garbage_dispatched_{event_type}"

def garbage_request_router(path):
    return f"garbage_routed_{path}"
""",
    "unrelated_dir/garbage_models.py": """class GarbageModelAlpha:
    garbage_field_one = "garbage_model_alpha_field_one"
    garbage_field_two = "garbage_model_alpha_field_two"

    def garbage_model_method_alpha(self):
        return "garbage_model_alpha_method"

class GarbageModelBeta:
    garbage_field_one = "garbage_model_beta_field_one"
    garbage_field_two = "garbage_model_beta_field_two"
    garbage_field_three = "garbage_model_beta_field_three"

    def garbage_model_method_beta(self):
        return "garbage_model_beta_method"

class GarbageModelGamma:
    garbage_field_one = "garbage_model_gamma_field_one"

    def garbage_model_method_gamma(self):
        return "garbage_model_gamma_method"

    def garbage_model_validate_gamma(self, data):
        return "garbage_model_gamma_validated"
""",
    "unrelated_dir/garbage_api.py": """def garbage_api_endpoint_alpha(request):
    return {"garbage_response": "alpha"}

def garbage_api_endpoint_beta(request):
    return {"garbage_response": "beta"}

def garbage_api_endpoint_gamma(request):
    return {"garbage_response": "gamma"}

def garbage_api_endpoint_delta(request):
    return {"garbage_response": "delta"}

class GarbageApiController:
    def garbage_get_all(self):
        return "garbage_api_get_all"

    def garbage_get_one(self, id):
        return f"garbage_api_get_{id}"

    def garbage_create(self, data):
        return "garbage_api_created"

    def garbage_update(self, id, data):
        return f"garbage_api_updated_{id}"

    def garbage_delete(self, id):
        return f"garbage_api_deleted_{id}"
""",
    "unrelated_dir/garbage_validators.py": """def validate_garbage_input_alpha(value):
    return "garbage_input_alpha_valid"

def validate_garbage_input_beta(value):
    return "garbage_input_beta_valid"

def validate_garbage_input_gamma(value):
    return "garbage_input_gamma_valid"

class GarbageValidatorAlpha:
    def validate(self, data):
        return "garbage_validator_alpha_result"

class GarbageValidatorBeta:
    def validate(self, data):
        return "garbage_validator_beta_result"

    def validate_strict(self, data):
        return "garbage_validator_beta_strict"
""",
    "unrelated_dir/garbage_unrelated.yaml": """zxcvb_settings:
  zxcvb_option_alpha: garbage_yaml_alpha_value
  zxcvb_option_beta: garbage_yaml_beta_value
  zxcvb_option_gamma: garbage_yaml_gamma_value

zxcvb_features:
  zxcvb_feature_one: true
  zxcvb_feature_two: false
""",
}

GARBAGE_MARKERS = [
    "garbage_marker_12345",
    "unused_marker_67890",
    "class_garbage_marker",
    "garbage_alpha_constant",
    "garbage_beta_constant",
    "js_garbage_marker_abc",
    "js_const_garbage",
    "completely_unrelated_garbage_function",
    "another_unused_helper",
    "UnusedGarbageClass",
    "unusedJsGarbage",
    "GARBAGE_JS_CONST",
    "garbage_helper_alpha_value",
    "garbage_helper_beta_value",
    "garbage_helper_gamma_value",
    "garbage_helper_delta_value",
    "garbage_helper_epsilon_value",
    "garbage_gamma_constant",
    "garbage_delta_constant",
    "garbage_epsilon_constant",
    "garbage_zeta_constant",
    "garbage_eta_constant",
    "garbage_theta_constant",
    "garbage_iota_constant",
    "garbage_kappa_constant",
    "js_garbage_alpha_marker",
    "js_garbage_beta_marker",
    "js_garbage_gamma_marker",
    "garbage_config_value_1",
    "garbage_config_value_2",
    "garbage_config_value_3",
    "GarbageTypeAlpha",
    "GarbageTypeBeta",
    "GarbageTypeGamma",
    "garbage_type_alpha_one",
    "garbage_type_beta_one",
    "garbage_type_gamma_one",
    "GarbageServiceAlpha",
    "GarbageServiceBeta",
    "GarbageServiceGamma",
    "garbage_service_alpha",
    "garbage_service_beta",
    "garbage_service_gamma",
    "garbage_validation_alpha",
    "garbage_validation_beta",
    "garbage_transform_alpha",
    "garbage_execution_gamma",
    "standalone_garbage_one",
    "standalone_garbage_two",
    "standalone_garbage_three",
    "GarbageHandlerAlpha",
    "GarbageHandlerBeta",
    "GarbageHandlerGamma",
    "garbage_event_alpha_handled",
    "garbage_event_beta_handled",
    "garbage_event_gamma_handled",
    "garbage_request_alpha_processed",
    "garbage_request_beta_processed",
    "garbage_dispatched",
    "garbage_routed",
    "GarbageModelAlpha",
    "GarbageModelBeta",
    "GarbageModelGamma",
    "garbage_model_alpha_field",
    "garbage_model_beta_field",
    "garbage_model_gamma_field",
    "garbage_api_endpoint_alpha",
    "garbage_api_endpoint_beta",
    "garbage_api_endpoint_gamma",
    "garbage_api_endpoint_delta",
    "GarbageApiController",
    "garbage_api_get_all",
    "garbage_api_created",
    "garbage_api_updated",
    "garbage_api_deleted",
    "validate_garbage_input_alpha",
    "validate_garbage_input_beta",
    "validate_garbage_input_gamma",
    "GarbageValidatorAlpha",
    "GarbageValidatorBeta",
    "garbage_validator_alpha_result",
    "garbage_validator_beta_result",
    "garbage_validator_beta_strict",
    "garbage_yaml_alpha_value",
    "garbage_yaml_beta_value",
    "garbage_yaml_gamma_value",
    "zxcvb_settings",
    "zxcvb_features",
]
