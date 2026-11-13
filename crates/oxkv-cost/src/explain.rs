//! The stable explain record for one query plan.
//!
//! Explain reads plan metadata only. It never reads a data page and produces no
//! event. The canonical JSON form and the text form use one fixed field order.
//! Two runs with equal inputs produce equal bytes.

use core::fmt;

use crate::budget::{Admission, CostBudget, Verdict};
use crate::error::{CostCode, CostError};
use crate::plan::{COST_CONTRACT_VERSION, QueryPlan};
use crate::util::{hex, json_hex, json_string};

/// One explain record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainRecord {
    /// The query plan.
    pub plan: QueryPlan,
    /// The active budget.
    pub budget: CostBudget,
    /// The budget verdict.
    pub admission: Admission,
}

impl ExplainRecord {
    /// Builds one explain record from a plan and a budget.
    #[must_use]
    pub fn new(plan: QueryPlan, budget: CostBudget) -> Self {
        let admission = budget.evaluate(&plan);
        Self {
            plan,
            budget,
            admission,
        }
    }

    /// Returns the verdict.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.admission.verdict
    }

    /// Returns the cost error codes in check order.
    pub fn cost_errors(&self) -> impl Iterator<Item = CostCode> + '_ {
        self.admission.error_codes()
    }

    /// Renders the canonical JSON form.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        let inputs = self.plan.inputs();
        let statistics = inputs
            .statistics_version
            .map_or_else(|| "null".to_owned(), |version| version.get().to_string());
        let shard_entries: Vec<String> = self
            .plan
            .shards()
            .iter()
            .map(|shard| {
                let fields = [
                    field("shard_id", &json_hex(shard.shard_id.as_bytes())),
                    field("term_probes", &shard.term_probes.to_string()),
                    field("keys_examined", &shard.keys_examined().to_string()),
                    field("estimated_rows", &shard.estimated_rows.to_string()),
                    field("token_count", &shard.token_count.to_string()),
                    field("cost_milli", &shard.cost_milli().to_string()),
                ];
                object(&fields)
            })
            .collect();
        let plan_fields = [
            field(
                "manifest_revision",
                &inputs.manifest_revision.get().to_string(),
            ),
            field(
                "policy_version",
                &json_hex(inputs.policy_version.as_bytes()),
            ),
            field("map_version", &inputs.map_version.get().to_string()),
            field("statistics_version", &statistics),
            field(
                "policy_normalization",
                &json_string(self.plan.policy_normalization()),
            ),
            field("merge_shape", &json_string(self.plan.merge_shape().name())),
            field("fanout", &self.plan.fanout().to_string()),
            field("estimated_rows", &self.plan.estimated_rows().to_string()),
            field("token_budget", &self.plan.token_budget().to_string()),
            field("total_tokens", &self.plan.total_tokens().to_string()),
            field("per_shard", &array(&shard_entries)),
        ];
        let cost_fields = [
            field("total_shards", &self.plan.shard_count().to_string()),
            field("total_keys", &self.plan.total_keys().to_string()),
            field(
                "total_cost_milli",
                &self.plan.total_cost_milli().to_string(),
            ),
        ];
        let budget_fields = [
            field("max_cost_milli", &self.budget.max_cost_milli.to_string()),
            field("max_fanout", &self.budget.max_fanout.to_string()),
            field("max_tokens", &self.budget.max_tokens.to_string()),
        ];
        let verdict = self.verdict_json();
        let errors: Vec<String> = self
            .cost_errors()
            .map(|code| json_string(code.as_str()))
            .collect();
        let fields = [
            field("contract", &json_string(COST_CONTRACT_VERSION)),
            field("query_plan", &object(&plan_fields)),
            field("cost", &object(&cost_fields)),
            field("active_budget", &object(&budget_fields)),
            field("verdict", &verdict),
            field("cost_errors", &array(&errors)),
        ];
        object(&fields)
    }

    /// Renders the human-readable text form.
    #[must_use]
    pub fn to_text(&self) -> String {
        let inputs = self.plan.inputs();
        let statistics = inputs
            .statistics_version
            .map_or_else(|| "none".to_owned(), |version| version.get().to_string());
        let mut lines = vec![
            format!("contract={COST_CONTRACT_VERSION}"),
            format!("manifest_revision={}", inputs.manifest_revision.get()),
            format!("policy_version={}", hex(inputs.policy_version.as_bytes())),
            format!("map_version={}", inputs.map_version.get()),
            format!("statistics_version={statistics}"),
            format!("policy_normalization={}", self.plan.policy_normalization()),
            format!("merge_shape={}", self.plan.merge_shape().name()),
            format!("fanout={}", self.plan.fanout()),
            format!("estimated_rows={}", self.plan.estimated_rows()),
            format!("token_budget={}", self.plan.token_budget()),
            format!("total_tokens={}", self.plan.total_tokens()),
            format!("total_shards={}", self.plan.shard_count()),
            format!("total_keys={}", self.plan.total_keys()),
            format!("total_cost_milli={}", self.plan.total_cost_milli()),
            format!(
                "active_budget_max_cost_milli={}",
                self.budget.max_cost_milli
            ),
            format!("active_budget_max_fanout={}", self.budget.max_fanout),
            format!("active_budget_max_tokens={}", self.budget.max_tokens),
        ];
        for shard in self.plan.shards() {
            lines.push(format!(
                "shard={} term_probes={} keys_examined={} estimated_rows={} token_count={} cost_milli={}",
                hex(shard.shard_id.as_bytes()),
                shard.term_probes,
                shard.keys_examined(),
                shard.estimated_rows,
                shard.token_count,
                shard.cost_milli()
            ));
        }
        lines.push(format!("verdict={}", self.admission.verdict.name()));
        for code in self.cost_errors() {
            lines.push(format!("cost_error={code}"));
        }
        lines.join("\n")
    }

    fn verdict_json(&self) -> String {
        match self.admission.verdict {
            Verdict::Admitted => object(&[field("status", &json_string("admitted"))]),
            Verdict::Rejected => {
                let code = self
                    .admission
                    .first_error()
                    .map_or(CostCode::InternalInvariantViolation, CostError::code);
                object(&[
                    field("status", &json_string("rejected")),
                    field("code", &json_string(code.as_str())),
                ])
            }
        }
    }
}

impl fmt::Display for ExplainRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_text())
    }
}

fn field(name: &str, value: &str) -> String {
    let mut text = json_string(name);
    text.push(':');
    text.push_str(value);
    text
}

fn object(fields: &[String]) -> String {
    let mut text = String::from("{");
    text.push_str(&fields.join(","));
    text.push('}');
    text
}

fn array(items: &[String]) -> String {
    let mut text = String::from("[");
    text.push_str(&items.join(","));
    text.push(']');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{CostInputs, MergeShape, ShardPlan};
    use oxkv_contracts::{ManifestRevision, MapVersion, PolicyVersion, ShardId, StatisticsVersion};

    fn record() -> ExplainRecord {
        let inputs = CostInputs::new(
            ManifestRevision::new(7),
            PolicyVersion::from_bytes([0xab; 32]),
            MapVersion::new(3),
            Some(StatisticsVersion::new(2)),
        );
        let plan = QueryPlan::new(
            inputs,
            "table == accounts",
            MergeShape::Search,
            vec![
                ShardPlan::new(ShardId::new(vec![2]), 3, 200, 15, 30),
                ShardPlan::new(ShardId::new(vec![1]), 2, 100, 10, 20),
            ],
            500,
        )
        .expect("the plan is valid");
        ExplainRecord::new(plan, CostBudget::new(10_000, 4, 1000))
    }

    #[test]
    fn two_runs_produce_identical_bytes() {
        let record = record();
        assert_eq!(record.to_canonical_json(), record.to_canonical_json());
        assert_eq!(record.to_text(), record.to_text());
    }

    #[test]
    fn the_json_form_names_every_required_field() {
        let json = record().to_canonical_json();
        for name in [
            "contract",
            "manifest_revision",
            "policy_version",
            "map_version",
            "statistics_version",
            "policy_normalization",
            "merge_shape",
            "fanout",
            "estimated_rows",
            "token_budget",
            "total_tokens",
            "per_shard",
            "total_shards",
            "total_keys",
            "total_cost_milli",
            "active_budget",
            "verdict",
            "cost_errors",
        ] {
            assert!(json.contains(name), "the explain JSON has no {name} field");
        }
    }

    #[test]
    fn a_rejected_plan_names_the_error_code() {
        let inputs = CostInputs::new(
            ManifestRevision::new(1),
            PolicyVersion::from_bytes([0; 32]),
            MapVersion::new(1),
            None,
        );
        let plan = QueryPlan::new(
            inputs,
            "term",
            MergeShape::Search,
            vec![ShardPlan::new(ShardId::new(vec![1]), 0, 5000, 1, 1)],
            10,
        )
        .expect("the plan is valid");
        let record = ExplainRecord::new(plan, CostBudget::new(100, 1, 100));
        assert_eq!(record.verdict(), Verdict::Rejected);
        assert_eq!(
            record.cost_errors().collect::<Vec<_>>(),
            vec![CostCode::QueryBudgetExceeded]
        );
        assert!(record.to_canonical_json().contains("query.budget_exceeded"));
        assert!(
            record
                .to_text()
                .contains("cost_error=query.budget_exceeded")
        );
    }
}
