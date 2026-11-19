//! Delivery default selection is made only after the existing canonical guard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryMode {
    Memory,
    Durable,
    SchemaAdmitted,
}
impl DeliveryMode {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "memory" => Ok(Self::Memory),
            "durable" => Ok(Self::Durable),
            "schema_admitted" => Ok(Self::SchemaAdmitted),
            _ => Err("task_delivery_mode"),
        }
    }
    pub fn requires_durable_profile(self) -> bool {
        self != Self::Memory
    }
    /// The executable must call this after require_compatible_schema with the
    /// matching principal. An error never selects memory or retries another role.
    pub fn after_canonical_guard(
        self,
        guard: Result<(), ()>,
        concurrency: i64,
    ) -> Result<bool, &'static str> {
        guard.map_err(|_| "task_canonical_schema_not_admitted")?;
        if self.requires_durable_profile() && concurrency <= 0 {
            return Err("task_worker_concurrency");
        }
        Ok(self.requires_durable_profile())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_requires_canonical_acceptance_and_cannot_downgrade() {
        let mode = DeliveryMode::parse("schema_admitted").unwrap();
        assert!(mode.requires_durable_profile());
        assert_eq!(
            mode.after_canonical_guard(Err(()), 1),
            Err("task_canonical_schema_not_admitted")
        );
        assert_eq!(mode.after_canonical_guard(Ok(()), 1), Ok(true));
        assert!(mode.after_canonical_guard(Ok(()), 0).is_err());
    }
    #[test]
    fn memory_remains_explicit_and_invalid_modes_do_not_guess() {
        assert_eq!(
            DeliveryMode::Memory.after_canonical_guard(Ok(()), 0),
            Ok(false)
        );
        for mode in ["auto", "", "legacy", "DURABLE"] {
            assert!(DeliveryMode::parse(mode).is_err());
        }
    }
}
