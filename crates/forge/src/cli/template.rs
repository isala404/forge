//! Template rendering utilities for scaffolding.
//!
//! Provides simple `{{var}}` replacement for template files.

use std::collections::HashMap;

/// Render a template by replacing `{{key}}` placeholders with values.
pub fn render(template: &str, vars: &HashMap<&str, &str>) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }
    result
}

/// Helper macro to create a HashMap of template variables.
#[macro_export]
macro_rules! template_vars {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut map = std::collections::HashMap::new();
        $(map.insert($key, $value);)*
        map
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_simple() {
        let template = "Hello, {{name}}!";
        let vars = template_vars!("name" => "World");
        assert_eq!(render(template, &vars), "Hello, World!");
    }

    #[test]
    fn test_render_multiple() {
        let template = "{{greeting}}, {{name}}! Welcome to {{place}}.";
        let vars = template_vars!(
            "greeting" => "Hello",
            "name" => "Alice",
            "place" => "Wonderland"
        );
        assert_eq!(
            render(template, &vars),
            "Hello, Alice! Welcome to Wonderland."
        );
    }

    #[test]
    fn test_render_repeated() {
        let template = "{{name}} said {{name}} is {{name}}.";
        let vars = template_vars!("name" => "Bob");
        assert_eq!(render(template, &vars), "Bob said Bob is Bob.");
    }

    #[test]
    fn test_render_no_vars() {
        let template = "No placeholders here.";
        let vars = HashMap::new();
        assert_eq!(render(template, &vars), "No placeholders here.");
    }

    #[test]
    fn test_render_missing_var() {
        let template = "Hello, {{name}}!";
        let vars = HashMap::new();
        assert_eq!(render(template, &vars), "Hello, {{name}}!");
    }
}
