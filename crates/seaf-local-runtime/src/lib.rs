pub fn runtime_component() -> &'static str {
    "seaf-local-runtime"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_runtime_component_name() {
        assert_eq!(runtime_component(), "seaf-local-runtime");
    }
}
