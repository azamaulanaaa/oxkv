# oxkv-policy

`oxkv-policy` holds the canonical policy AST v1 for oxkv. It provides one
production evaluator for read scope and write validation.

## Surface

- The AST has paths, literals, comparison, boolean connectives, membership,
  existence, and a rejected regex node.
- The canonical bytes are deterministic. The policy version is the
  domain-separated BLAKE3 hash of those bytes.
- The evaluator binds `before` and `after`. Read scope binds both sides to the
  same row. Write validation binds the stored value and the proposed value.
- Evaluation is per row. A batch verdict is the conjunction of the row
  verdicts.
- A step budget, a depth budget, and a fixed memory arena bound each row. A
  failure returns `policy.budget_exhausted` and maps to `Rejected`.
- The explanation bytes carry the row key, rule identity, verdict, reason code,
  and evaluated path. The bytes are stable across native and `wasm32`.

The crate uses no clock, randomness, I/O, host inspection, state, regular
expressions, floating point, or `unsafe`.

## Example

```rust
use oxkv_policy::{
    Budget, CompareOp, Expr, Literal, Path, PathRoot, Policy, Row, RuleId,
};

let rule = RuleId::new("accounts.owner").expect("the rule identity is valid");
let expr = Expr::Compare {
    op: CompareOp::Eq,
    left: Box::new(Expr::Path(Path::field(PathRoot::Before, "owner"))),
    right: Box::new(Expr::Literal(Literal::Text("alice".to_owned()))),
};
let policy = Policy::new(rule, Budget::standard(), expr).expect("the policy is bounded");
let row = Row::new().with_field("owner", Literal::Text("alice".to_owned()));
assert!(policy.evaluate_read(b"row-1", &row).is_allow());
```

## Golden vectors

`tests/vectors/policy-v1.json` holds the pinned vectors. The test runner
executes the production evaluator and a test-only differential oracle. The
oracle is in the test tree only, so no production path can reach it.
