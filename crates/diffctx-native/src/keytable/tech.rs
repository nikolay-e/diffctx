//! How the technologies the key table meets spell the same setting in
//! different places, each rule with the upstream documentation that states it.
//! The rules are data a reader can check; `keys_extract.rs` applies the ones
//! that name a transformation.

use crate::canon::canon_key;

#[allow(
    dead_code,
    reason = "the documentation a reader and the table's test check; only env_prefix acts"
)]
pub(crate) struct TechRow {
    pub technology: &'static str,
    /// Where the spelling occurs.
    pub shape: &'static str,
    /// What the documentation says, and what the table does with it.
    pub rule: &'static str,
    pub url: &'static str,
    /// The prefix the image puts before a broker property in its variable.
    pub env_prefix: Option<&'static str>,
}

pub(crate) const TECH_TABLE: &[TechRow] = &[
    TechRow {
        technology: "Spring Boot",
        shape: "environment variable <-> configuration property",
        rule: "dots become underscores, dashes are removed, upper case: spring.main.log-startup-info is SPRING_MAIN_LOGSTARTUPINFO; a property is keyed by its canonical form and by that dashless form",
        url: "https://docs.spring.io/spring-boot/reference/features/external-config.html#features.external-config.typesafe-configuration-properties.relaxed-binding.environment-variables",
        env_prefix: None,
    },
    TechRow {
        technology: "Spring Boot",
        shape: "${name:default} inside a property value",
        rule: "the name is looked up in the Environment, so `${demo.item-price}` also reads DEMO_ITEMPRICE; the placeholder reads the property and the environment variable, the default is ignored",
        url: "https://docs.spring.io/spring-boot/reference/features/external-config.html#features.external-config.files.property-placeholders",
        env_prefix: None,
    },
    TechRow {
        technology: "Django (django-environ)",
        shape: "env('KEY'), env.<cast>('KEY') in a settings module",
        rule: "the call reads the environment variable KEY; read only in files whose path names settings or config, where a function called env is a settings helper",
        url: "https://django-environ.readthedocs.io/en/latest/quickstart.html",
        env_prefix: None,
    },
    TechRow {
        technology: "Django",
        shape: "DJANGO_SETTINGS_MODULE",
        rule: "the settings module is chosen through the environment variable DJANGO_SETTINGS_MODULE; the generic os.environ read already covers it, and the documentation names no other environment convention for individual settings",
        url: "https://docs.djangoproject.com/en/stable/topics/settings/",
        env_prefix: None,
    },
    TechRow {
        technology: "nginx (official image)",
        shape: "*.template files under /etc/nginx/templates",
        rule: "envsubst runs on every file with the .template suffix and writes the result without the suffix to /etc/nginx/conf.d, so ${VAR} and $VAR in a template read the container's environment",
        url: "https://hub.docker.com/_/nginx",
        env_prefix: None,
    },
    TechRow {
        technology: "Kafka (Confluent images)",
        shape: "environment variable <-> broker property",
        rule: "prefix KAFKA_, upper case, a period becomes an underscore: metric.reporters is KAFKA_METRIC_REPORTERS; KAFKA_X_Y is also keyed x_y, the property's canonical form",
        url: "https://docs.confluent.io/platform/current/installation/docker/config-reference.html",
        env_prefix: Some("kafka_"),
    },
    TechRow {
        technology: "Kafka (Bitnami image)",
        shape: "environment variable <-> server.properties key",
        rule: "a variable beginning KAFKA_CFG_ is mapped to its Kafka key: KAFKA_CFG_BACKGROUND_THREADS sets background.threads; KAFKA_CFG_X_Y is also keyed x_y",
        url: "https://github.com/bitnami/containers/blob/main/bitnami/kafka/README.md",
        env_prefix: Some("kafka_cfg_"),
    },
    TechRow {
        technology: "Redis (official image)",
        shape: "redis.conf <-> environment",
        rule: "none: the image reads redis.conf or command-line arguments and no configuration environment variable, so a Redis setting meets an environment variable only by the application's own name (REDIS_URL, REDIS_HOST)",
        url: "https://hub.docker.com/_/redis",
        env_prefix: None,
    },
    TechRow {
        technology: "Redis (Bitnami image)",
        shape: "named environment variables",
        rule: "REDIS_PASSWORD, REDIS_PORT_NUMBER, REDIS_DISABLE_COMMANDS and ALLOW_EMPTY_PASSWORD are listed one by one and the documentation gives no general mapping from redis.conf directives, so they are plain environment keys",
        url: "https://github.com/bitnami/containers/blob/main/bitnami/redis/README.md",
        env_prefix: None,
    },
];

/// The keys a configuration property answers to: its canonical form and the
/// form an environment variable gives it under Spring's relaxed binding
/// (dots to underscores, dashes removed).
pub(crate) fn property_keys(property: &str) -> Vec<String> {
    let canonical = canon_key(property);
    let relaxed = property
        .replace('.', "_")
        .replace('-', "")
        .to_ascii_lowercase();
    let relaxed = canon_key(&relaxed);
    let mut keys = vec![canonical];
    if keys[0] != relaxed {
        keys.push(relaxed);
    }
    keys.retain(|k| k.len() >= 2);
    keys
}

/// The property key a Kafka image variable stands for: the key without the
/// longest prefix a row of the table names (`kafka_cfg_` for Bitnami, `kafka_`
/// for Confluent).
pub(crate) fn kafka_property(env_key: &str) -> Option<String> {
    let rest = TECH_TABLE
        .iter()
        .filter_map(|row| env_key.strip_prefix(row.env_prefix?))
        .min_by_key(|rest| rest.len())?;
    (rest.len() >= 3).then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_names_its_upstream_page() {
        for row in TECH_TABLE {
            assert!(row.url.starts_with("https://"), "{}", row.technology);
            assert!(!row.shape.is_empty() && !row.rule.is_empty());
        }
    }

    #[test]
    fn spring_binds_a_property_to_the_variable_the_documentation_names() {
        // spring.main.log-startup-info <-> SPRING_MAIN_LOGSTARTUPINFO
        let property = property_keys("spring.main.log-startup-info");
        assert!(property.contains(&canon_key("SPRING_MAIN_LOGSTARTUPINFO")));
        assert!(property.contains(&canon_key("SPRING_MAIN_LOG_STARTUP_INFO")));
        // ${demo.item-price} <-> DEMO_ITEMPRICE
        assert!(property_keys("demo.item-price").contains(&canon_key("DEMO_ITEMPRICE")));
        assert!(property_keys("demo.itemPrice").contains(&canon_key("DEMO_ITEMPRICE")));
    }

    #[test]
    fn kafka_images_prefix_the_property_and_redis_has_no_convention() {
        assert_eq!(
            kafka_property("kafka_advertised_listeners").as_deref(),
            Some("advertised_listeners")
        );
        assert_eq!(
            kafka_property("kafka_cfg_auto_create_topics_enable").as_deref(),
            Some("auto_create_topics_enable")
        );
        assert_eq!(canon_key("advertised.listeners"), "advertised_listeners");
        assert_eq!(kafka_property("redis_password"), None);
    }
}
