//! Feature modules - each owns its tools end to end (schema, CLI
//! spec, markdown renderer, handler binding). Add a new feature by
//! creating a module with a pub fn tools() -> Vec<ToolDef> and one
//! extend line here.

use crate::registry::ToolDef;

mod calendar;
mod courses;
mod digest;
mod fbf;
mod files;
mod grades;
mod kaltura;
mod messages;
mod pcv;
mod roster;

/// Assemble the full registry. Called once (OnceLock in the parent module).
pub fn build() -> Vec<ToolDef> {
    let mut out = Vec::new();
    out.extend(courses::tools());
    out.extend(files::tools());
    out.extend(calendar::tools());
    out.extend(grades::tools());
    out.extend(messages::tools());
    out.extend(roster::tools());
    out.extend(digest::tools());
    out.extend(fbf::tools());
    out.extend(pcv::tools());
    out.extend(kaltura::tools());
    out
}
