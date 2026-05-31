use std::path::Path;

use anyhow::Result;
use deno_core::{FastString, JsRuntime, RuntimeOptions};

/// A lightweight wrapper around a Deno/V8 JavaScript runtime.
///
/// Used for schema loading (evaluating the JS schema folder) and will serve as
/// the host for JS plugins. Keeping this abstraction isolated means the rest of
/// the codebase never touches deno_core directly; only this module needs to
/// change if the deno_core API evolves.
pub struct ScriptRuntime {
    inner: JsRuntime,
}

impl ScriptRuntime {
    pub fn new() -> Result<Self> {
        let inner = JsRuntime::new(RuntimeOptions::default());
        Ok(Self { inner })
    }

    /// Execute a JavaScript snippet, discarding the return value.
    pub fn exec(&mut self, name: &'static str, src: impl Into<String>) -> Result<()> {
        self.inner.execute_script(name, FastString::from(src.into()))?;
        Ok(())
    }

    /// Load and execute a JavaScript file from disk.
    /// The script name shown in error messages will be the filename.
    pub fn exec_file(&mut self, path: &Path) -> Result<()> {
        let src = std::fs::read_to_string(path)?;
        self.inner
            .execute_script("<file>", FastString::from(src))
            .with_context(|| format!("in {}", path.display()))?;
        Ok(())
    }

    /// Evaluate a JavaScript expression and return its value as parsed JSON.
    /// The expression is wrapped in `JSON.stringify(...)` before evaluation.
    pub fn eval_json(&mut self, expr: &str) -> Result<serde_json::Value> {
        let src = format!("JSON.stringify({expr})");
        let global = self.inner.execute_script("<eval>", FastString::from(src))?;
        let scope = &mut self.inner.handle_scope();
        let local = deno_core::v8::Local::new(scope, global);
        let json_str = local.to_rust_string_lossy(scope);
        Ok(serde_json::from_str(&json_str)?)
    }
}

use anyhow::Context as _;
