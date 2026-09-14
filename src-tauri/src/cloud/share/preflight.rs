pub use super::CloudTransferCapacity;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capacity_accepts_integer_and_decimal_string_fields() {
        assert_eq!(
            CloudTransferCapacity::from_json(&json!(100), &json!(40), false).unwrap(),
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }
        );
        assert_eq!(
            CloudTransferCapacity::from_json(&json!("100"), &json!("40"), false).unwrap(),
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }
        );
        assert_eq!(
            CloudTransferCapacity::Limited { total_bytes: 100, used_bytes: 40 }.available_bytes(),
            Some(60)
        );
    }

    #[test]
    fn capacity_accepts_only_the_explicit_unlimited_sentinel() {
        assert_eq!(
            CloudTransferCapacity::from_json(&json!(-1), &json!(40), true).unwrap(),
            CloudTransferCapacity::Unlimited
        );
        assert_eq!(CloudTransferCapacity::Unlimited.available_bytes(), None);
        assert!(CloudTransferCapacity::from_json(&json!(-1), &json!(40), false).is_err());
    }

    #[test]
    fn capacity_rejects_missing_fractional_negative_and_inverted_values() {
        for (total, used) in [
            (json!(null), json!(1)),
            (json!(10.5), json!(1)),
            (json!(10), json!(-1)),
            (json!(10), json!(11)),
            (json!("+10"), json!("1")),
            (json!("ten"), json!("1")),
        ] {
            assert!(CloudTransferCapacity::from_json(&total, &used, false).is_err());
        }
    }
}
