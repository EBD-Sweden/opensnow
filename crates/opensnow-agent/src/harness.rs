use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use opensnow_core::OpenSnowEngine;
use serde_json::Value;
use tracing::debug;

/// Shared runtime context passed to all tools and tasks.
///
/// This mirrors the harness patterns used in claw-code: a thin context object
/// that holds the engine and minimal identity/warehouse info.
pub struct AgentContext {
    pub engine: Arc<OpenSnowEngine>,
    pub warehouse: String,
    pub user: Option<String>,
}

impl AgentContext {
    pub fn new(
        engine: impl Into<Arc<OpenSnowEngine>>,
        warehouse: impl Into<String>,
        user: Option<String>,
    ) -> Self {
        Self {
            engine: engine.into(),
            warehouse: warehouse.into(),
            user,
        }
    }
}

/// A tool is a small, reusable capability that an agent can call.
/// Tools operate on the AgentContext and arbitrary JSON parameters.
///
/// Note: `?Send` is used because `AgentContext` holds an `OpenSnowEngine`
/// which contains a non-`Sync` SQLite connection. All tool futures are
/// therefore single-threaded; the tools themselves remain `Send + Sync`.
#[async_trait::async_trait(?Send)]
pub trait Tool: Send + Sync + 'static {
    /// Stable name used by agents (and MCP) to reference this tool.
    fn name(&self) -> &'static str;

    /// Invoke the tool with the given context and parameters, returning JSON.
    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value>;
}

/// A high-level agent task that orchestrates multiple tool calls to achieve
/// a goal (e.g., analytics schema refactor).
#[async_trait::async_trait(?Send)]
pub trait AgentTask: Send + 'static {
    /// Stable identifier for this task.
    fn id(&self) -> &'static str;

    /// Execute the task using the provided runtime and context.
    async fn run(&mut self, runtime: &AgentRuntime, ctx: &mut AgentContext) -> Result<()>;
}

/// Central registry and dispatcher for tools.
pub struct AgentRuntime {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl AgentRuntime {
    /// Create a new, empty runtime.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool with the runtime.
    pub fn register_tool<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name();
        debug!("registering agent tool: {}", name);
        self.tools.insert(name, Arc::new(tool));
    }

    /// Invoke a tool by name.
    pub async fn invoke_tool(
        &self,
        name: &str,
        ctx: &mut AgentContext,
        params: Value,
    ) -> Result<Value> {
        if let Some(tool) = self.tools.get(name) {
            tool.invoke(ctx, params).await
        } else {
            anyhow::bail!("unknown tool: {}", name)
        }
    }

    /// Check if a tool is registered.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use opensnow_core::EngineConfig;

    struct EchoTool;

    #[async_trait::async_trait(?Send)]
    impl Tool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }

        async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
            Ok(params)
        }
    }

    struct SimpleTask {
        pub called: bool,
    }

    #[async_trait::async_trait(?Send)]
    impl AgentTask for SimpleTask {
        fn id(&self) -> &'static str {
            "simple_task"
        }

        async fn run(&mut self, runtime: &AgentRuntime, ctx: &mut AgentContext) -> Result<()> {
            let payload = serde_json::json!({ "message": "hello" });
            let out = runtime.invoke_tool("echo", ctx, payload.clone()).await?;
            assert_eq!(out, payload);
            self.called = true;
            Ok(())
        }
    }

    #[tokio::test]
    async fn runtime_can_register_and_invoke_tool() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let mut ctx = AgentContext::new(engine, "default", None);

        let mut runtime = AgentRuntime::new();
        runtime.register_tool(EchoTool);

        assert!(runtime.has_tool("echo"));

        let payload = serde_json::json!({ "k": 1 });
        let out = runtime
            .invoke_tool("echo", &mut ctx, payload.clone())
            .await
            .unwrap();
        assert_eq!(out, payload);
    }

    #[tokio::test]
    async fn agent_task_can_use_runtime_and_tools() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let mut ctx = AgentContext::new(engine, "default", Some("test-user".to_string()));

        let mut runtime = AgentRuntime::new();
        runtime.register_tool(EchoTool);

        let mut task = SimpleTask { called: false };
        task.run(&runtime, &mut ctx).await.unwrap();
        assert!(task.called);
    }
}
