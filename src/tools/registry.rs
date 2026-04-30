//! Thread-safe tool registry with deny-list filtering.
//!
//! See [`ToolRegistry`] for details.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tracing::debug;

use crate::llm::types::ToolDef;
use crate::tools::traits::Tool;

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

/// Registry of LLM-invokable tools with deny-list filtering.
///
/// Tools are registered by name, filtered by `--disallowed-tools`, and exposed
/// to the LLM provider as a list of [`ToolDef`] structs for API requests.
pub struct ToolRegistry {
    /// All registered tools, keyed by name.
    ///
    /// Wrapped in `Arc<Mutex<>>` so that `&self` methods (`get`, `tool_defs`,
    /// `visible_count`) can be called concurrently from multiple async tasks
    /// (agent loop, LSP callbacks).  `register()` takes `&mut self` because
    /// dynamic tool registration is an infrequent mutation — callers that
    /// need shared mutable access can wrap the entire registry in
    /// `Arc<Mutex<ToolRegistry>>`, and the inner `Mutex` still allows
    /// concurrent reads when the outer lock is held for writes.
    tools: Arc<Mutex<HashMap<String, Arc<dyn Tool>>>>,
    /// Set of tool names to hide from the LLM.
    disallowed: HashSet<String>,
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tools = self.tools.lock().expect("tool registry lock poisoned");
        let visible = tools
            .keys()
            .filter(|name| !self.disallowed.contains(name.as_str()))
            .count();
        f.debug_struct("ToolRegistry")
            .field("tool_count", &tools.len())
            .field("visible_count", &visible)
            .field("disallowed", &self.disallowed)
            .finish_non_exhaustive()
    }
}

impl ToolRegistry {
    /// Create a new registry, filtering out disallowed tools.
    ///
    /// Tools whose names appear in `disallowed` are excluded from the LLM-facing
    /// tool list but are still stored internally so they can be re-enabled later.
    pub fn new(tools: Vec<Box<dyn Tool>>, disallowed: Vec<String>) -> Self {
        let mut map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        for tool in tools {
            let name = tool.name().to_owned();
            debug!(tool = %name, "registering tool");
            map.insert(name, Arc::from(tool));
        }

        let disallowed: HashSet<String> = disallowed.into_iter().collect();

        if !disallowed.is_empty() {
            debug!(?disallowed, "tools hidden from LLM via deny-list");
        }

        ToolRegistry {
            tools: Arc::new(Mutex::new(map)),
            disallowed,
        }
    }

    /// Look up a tool by name for dispatch.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .lock()
            .expect("tool registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// List all tool definitions visible to the LLM (disallowed tools excluded).
    pub fn tool_defs(&self) -> Vec<ToolDef> {
        let tools = self.tools.lock().expect("tool registry lock poisoned");
        let mut defs: Vec<ToolDef> = tools
            .iter()
            .filter(|(name, _)| !self.disallowed.contains(name.as_str()))
            .map(|(_, tool)| ToolDef {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                input_schema: tool.parameters_schema(),
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Number of tools visible to the LLM (after deny-list filtering).
    pub fn visible_count(&self) -> usize {
        let tools = self.tools.lock().expect("tool registry lock poisoned");
        tools
            .keys()
            .filter(|name| !self.disallowed.contains(name.as_str()))
            .count()
    }

    /// Register a new tool dynamically.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_owned();
        debug!(tool = %name, "registering tool (dynamic)");
        self.tools
            .lock()
            .expect("tool registry lock poisoned")
            .insert(name, Arc::from(tool));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::{ToolContext, ToolFuture, ToolResult};
    use serde_json::json;

    /// Minimal stub tool for registry tests.
    ///
    /// Satisfies the full [`Tool`] trait contract but the `execute` method is
    /// never invoked by registry-only tests (register/get/filter).
    struct StubTool {
        name: &'static str,
        desc: &'static str,
        schema: serde_json::Value,
        readonly: bool,
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters_schema(&self) -> serde_json::Value {
            self.schema.clone()
        }
        fn is_read_only(&self) -> bool {
            self.readonly
        }
        fn execute<'a>(
            &'a self,
            _input: serde_json::Value,
            _ctx: &'a ToolContext,
        ) -> ToolFuture<'a> {
            Box::pin(async move { Ok(ToolResult::ok("stub")) })
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = ToolRegistry::new(vec![], vec![]);
        let tool = Box::new(StubTool {
            name: "read",
            desc: "Reads files",
            schema: json!({}),
            readonly: true,
        });
        reg.register(tool);
        assert!(reg.get("read").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn deny_list_filters_tools() {
        let t1 = Box::new(StubTool {
            name: "read",
            desc: "r",
            schema: json!({}),
            readonly: true,
        });
        let t2 = Box::new(StubTool {
            name: "write",
            desc: "w",
            schema: json!({}),
            readonly: false,
        });
        let t3 = Box::new(StubTool {
            name: "exec",
            desc: "e",
            schema: json!({}),
            readonly: false,
        });

        let reg = ToolRegistry::new(vec![t1, t2, t3], vec!["exec".to_string()]);

        assert_eq!(reg.visible_count(), 2);
        let defs = reg.tool_defs();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(!names.contains(&"exec"));
    }

    #[test]
    fn tool_defs_include_schemas() {
        let schema = json!({"type": "object", "properties": {"path": {"type": "string"}}});
        let tool = Box::new(StubTool {
            name: "cat",
            desc: "view file",
            schema: schema.clone(),
            readonly: true,
        });
        let reg = ToolRegistry::new(vec![tool], vec![]);
        let defs = reg.tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "cat");
        assert_eq!(defs[0].input_schema, schema);
    }

    #[test]
    fn empty_registry_is_valid() {
        let reg = ToolRegistry::new(vec![], vec![]);
        assert_eq!(reg.visible_count(), 0);
        assert!(reg.tool_defs().is_empty());
    }

    #[test]
    fn disallowed_tool_not_in_defs() {
        let tool = Box::new(StubTool {
            name: "danger",
            desc: "do not use",
            schema: json!({}),
            readonly: false,
        });
        let reg = ToolRegistry::new(vec![tool], vec!["danger".to_string()]);
        assert_eq!(reg.visible_count(), 0);
        assert!(reg.tool_defs().is_empty());
    }
}
