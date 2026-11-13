//! Name conversions between WIT (kebab-case), Rust/Tauri (snake_case) and
//! JavaScript (camelCase) conventions.

/// Split an identifier written in any of the supported conventions into
/// lowercase words.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | ' ' | '.') {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
        } else if c.is_uppercase() {
            if prev_lower && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.extend(c.to_lowercase());
            prev_lower = false;
        } else {
            current.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub(crate) fn to_kebab(name: &str) -> String {
    words(name).join("-")
}

pub(crate) fn to_snake(name: &str) -> String {
    words(name).join("_")
}

pub(crate) fn to_camel(name: &str) -> String {
    let mut out = String::new();
    for (i, w) in words(name).into_iter().enumerate() {
        if i == 0 {
            out.push_str(&w);
        } else {
            let mut chars = w.chars();
            if let Some(f) = chars.next() {
                out.extend(f.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

/// Names to try, in order, when looking up `name`.
pub(crate) fn candidates(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    for c in [to_kebab(name), to_snake(name), to_camel(name)] {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(to_kebab("ageYears"), "age-years");
        assert_eq!(to_kebab("age_years"), "age-years");
        assert_eq!(to_camel("age-years"), "ageYears");
        assert_eq!(to_snake("ageYears"), "age_years");
        assert_eq!(to_camel("greet"), "greet");
        assert_eq!(to_kebab("get-v2-items"), "get-v2-items");
        assert_eq!(
            candidates("get_user"),
            vec!["get_user", "get-user", "getUser"]
        );
    }
}
