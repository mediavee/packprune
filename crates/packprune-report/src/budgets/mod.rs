//! Budgets that fail optimise and check. The engine maps `[budgets]` to [`Budgets`]: this crate does not depend on
//! `packprune-config`.

const MIB: u64 = 1 << 20;

/// Each limit is off when `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budgets {
    pub zip_bytes: Option<u64>,
    pub atlas_gpu_mib: Option<u64>,
    pub font_mib: Option<u64>,
}

impl Budgets {
    /// Whether a limit needs the client's memory cost, which is the expensive measure.
    pub fn needs_cost(&self) -> bool {
        self.atlas_gpu_mib.is_some() || self.font_mib.is_some()
    }
}

/// What the run measured on its output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    pub zip_bytes: u64,
    pub atlas_gpu_bytes: u64,
    pub font_bytes: u64,
}

/// One budget the output exceeds: exit 1. `limit` and `measured` are in the budget's own unit (`budget` is its
/// config key): bytes, or MiB with `measured` rounded up, so `measured > limit` holds exactly when the output
/// is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exceeded {
    pub budget: &'static str,
    pub limit: u64,
    pub measured: u64,
}

/// The budgets `measured` exceeds, in the order of [`Budgets`]' fields.
pub fn check(budgets: &Budgets, measured: &Measured) -> Vec<Exceeded> {
    let mib = |bytes: u64| bytes.div_ceil(MIB);
    [
        ("zip_bytes", budgets.zip_bytes, measured.zip_bytes),
        (
            "atlas_gpu_mib",
            budgets.atlas_gpu_mib,
            mib(measured.atlas_gpu_bytes),
        ),
        ("font_mib", budgets.font_mib, mib(measured.font_bytes)),
    ]
    .into_iter()
    .filter_map(|(budget, limit, measured)| {
        let limit = limit?;
        (measured > limit).then_some(Exceeded {
            budget,
            limit,
            measured,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured() -> Measured {
        Measured {
            zip_bytes: 1_000,
            atlas_gpu_bytes: 100 * MIB,
            font_bytes: 2 * MIB + 1,
        }
    }

    #[test]
    fn no_budget_exceeds_nothing() {
        assert!(check(&Budgets::default(), &measured()).is_empty());
    }

    #[test]
    fn a_limit_equal_to_the_measure_is_within() {
        let budgets = Budgets {
            zip_bytes: Some(1_000),
            atlas_gpu_mib: Some(100),
            font_mib: Some(3),
        };
        assert!(check(&budgets, &measured()).is_empty());
    }

    #[test]
    fn one_byte_over_a_mib_limit_exceeds_it() {
        let budgets = Budgets {
            font_mib: Some(2),
            ..Budgets::default()
        };
        assert_eq!(
            check(&budgets, &measured()),
            vec![Exceeded {
                budget: "font_mib",
                limit: 2,
                measured: 3
            }]
        );
    }

    #[test]
    fn every_exceeded_budget_is_listed_in_field_order() {
        let budgets = Budgets {
            zip_bytes: Some(999),
            atlas_gpu_mib: Some(99),
            font_mib: Some(1),
        };
        let names: Vec<&str> = check(&budgets, &measured())
            .iter()
            .map(|exceeded| exceeded.budget)
            .collect();
        assert_eq!(names, ["zip_bytes", "atlas_gpu_mib", "font_mib"]);
    }
}
