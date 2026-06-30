pub fn framework_name() -> &'static str {
    "Self-Evolving Application Framework"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_framework_name() {
        assert_eq!(framework_name(), "Self-Evolving Application Framework");
    }
}
