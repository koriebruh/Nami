//! Escalation policies: the pure planner (`plan`), the policies, their steps
//! and targets (`model`), their routes, and the tick that walks incidents
//! through them (`run`).
pub mod model;
pub mod plan;
pub mod routes;
pub mod run;
