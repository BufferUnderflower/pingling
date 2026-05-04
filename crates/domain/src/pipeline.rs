//! Typed lifecycle hook pipeline — the core composition framework.
//!
//! Every operation flows through a [`Pipeline`] that runs a sequence of
//! [`Hook`]s around a terminal [`Handler`]. Hooks declare their intent by
//! implementing only the phase methods they care about — all others default
//! to no-ops. Advanced cases use [`WrapHook`] for full Tower-style wrapping
//! with a `next` reference.
//!
//! # Conceptual model
//!
//! ```text
//!  ┌────────────────────────────────────────────────────────────────────┐
//!  │  WrapHook A  (optional — for retry / circuit-break / caching)      │
//!  │  ┌──────────────────────────────────────────────────────────────┐  │
//!  │  │  WrapHook B                                                  │  │
//!  │  │  ┌────────────────────────────────────────────────────────┐  │  │
//!  │  │  │  before_1 ──► before_2 ──►  HANDLER  ──► after_2 ──► after_1 │
//!  │  │  │                                 │                      │  │  │
//!  │  │  │                         on_error_1 ──► on_error_2      │  │  │
//!  │  │  └────────────────────────────────────────────────────────┘  │  │
//!  │  └──────────────────────────────────────────────────────────────┘  │
//!  └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Hooks — standard extension point
//!
//! Implement [`Hook<Op>`] and override only the methods you need:
//!
//! | Method | Phase | Can do | Execution order |
//! |--------|-------|--------|----------------|
//! | [`before`](Hook::before) | Pre-operation | Modify input, reject with `Err` | Registration order |
//! | [`after`](Hook::after) | Post-success | Modify output, reject with `Err` | Reverse registration order |
//! | [`on_error`](Hook::on_error) | Error path | Observe (read-only) | Registration order; **all always run** |
//!
//! ## Wrap hooks — advanced escape hatch
//!
//! Implement [`WrapHook<Op>`] when before/after/on_error are insufficient:
//! retry loops, response caching, circuit breakers, distributed tracing spans.
//!
//! A [`WrapHook`] receives a `next: &dyn Handler<Op>` that points to the full
//! hook chain + terminal handler. First registered = outermost wrapper.
//!
//! # Example
//!
//! ```
//! use pingling_domain::pipeline::*;
//! use pingling_domain::VpnError;
//!
//! pub struct Greet;
//! impl Operation for Greet {
//!     type Input  = String;
//!     type Output = String;
//!     fn name() -> &'static str { "greet" }
//! }
//!
//! struct Greeter;
//! impl Handler<Greet> for Greeter {
//!     fn handle(&self, name: String) -> Result<String, VpnError> {
//!         Ok(format!("Hello, {name}!"))
//!     }
//! }
//!
//! // A hook that uppercases the output
//! struct ShoutHook;
//! impl Hook<Greet> for ShoutHook {
//!     fn name(&self) -> &str { "shout" }
//!     fn after(&self, _input: &String, output: &mut String) -> Result<(), VpnError> {
//!         *output = output.to_uppercase();
//!         Ok(())
//!     }
//! }
//!
//! let mut pipeline = Pipeline::new(Box::new(Greeter));
//! pipeline.push_hook(Box::new(ShoutHook));
//! assert_eq!(pipeline.execute("world".into()).unwrap(), "HELLO, WORLD!");
//! ```

use crate::errors::VpnError;

// ---------------------------------------------------------------------------
// Operation
// ---------------------------------------------------------------------------

/// A typed operation with known input and output.
///
/// Each operation (connect, list outbounds, test latency, …) is a zero-sized
/// struct implementing this trait. The associated `Input` and `Output` types
/// give full compile-time safety: a `Hook<OpConnect>` cannot be mistakenly
/// registered on a `Pipeline<OpListOutbounds>`.
///
/// `Input` must be [`Clone`] because the pipeline snapshots it before passing
/// ownership to the handler. The snapshot lets `on_error` and `after` hooks
/// receive the **original mutated input** even after the handler has consumed it.
pub trait Operation: Send + Sync + 'static {
    /// Data flowing into the pipeline. Must be `Clone` for error callbacks.
    type Input: Send + Clone + 'static;
    /// Data flowing out on success.
    type Output: Send + 'static;
    /// Human-readable name used in logs and diagnostics.
    fn name() -> &'static str;
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Terminal handler — the core logic at the bottom of the pipeline.
///
/// For lifecycle operations (connect, disconnect), the handler calls `VpnCore`
/// methods. For capabilities (list outbounds), the handler is core-specific
/// (e.g. parses a sing-box config or calls the Clash REST API).
///
/// Every [`Pipeline`] has exactly one `Handler`. The handler sits at the
/// very bottom — every [`Hook`] and [`WrapHook`] runs around it.
///
/// `Handler` is also the type of the `next` argument in [`WrapHook::handle`],
/// which lets wrap hooks call the rest of the chain — including all inner hooks.
pub trait Handler<Op: Operation>: Send + Sync {
    /// Execute the operation.
    fn handle(&self, input: Op::Input) -> Result<Op::Output, VpnError>;
}

// ---------------------------------------------------------------------------
// Hook — standard extension point
// ---------------------------------------------------------------------------

/// Standard lifecycle hook — explicit before / after / on_error phases.
///
/// Override only the methods that matter for your hook. All three default
/// to no-ops so you never write boilerplate.
///
/// # Phase contract
///
/// ```text
/// [before_1 → before_2 → ...] → HANDLER → [after_N → ... → after_1]
///                                     │
///                               [on_error_1 → on_error_2 → ...]
/// ```
///
/// - **`before`**: runs before the handler, in registration order. May modify
///   `input` in-place or return `Err` to abort. If any `before` returns `Err`,
///   the handler is never called and all `on_error` hooks fire.
///
/// - **`after`**: runs after the handler **succeeds**, in *reverse* registration
///   order (last-in, first-out — like unwinding a call stack). May modify
///   `output` in-place or return `Err` to turn a success into a failure.
///   Remaining `after` hooks are skipped and all `on_error` hooks fire.
///
/// - **`on_error`**: runs when the operation fails (handler error, `before`
///   rejection, or `after` rejection). Read-only. **All `on_error` hooks always
///   run**, in registration order, regardless of what the others do.
///
/// # Naming convention
///
/// `"scope:name"` — e.g. `"builtin:logging"`, `"component:geo-filter"`, `"my-plugin:auth"`.
pub trait Hook<Op: Operation>: Send + Sync {
    /// Unique name for diagnostics, deduplication, and tracing.
    fn name(&self) -> &str;

    /// Runs *before* the handler, in hook registration order.
    ///
    /// Receives `&mut input` — inspect or rewrite any field freely.
    /// Return `Err` to abort: the handler is skipped and all `on_error`
    /// hooks fire with `input` as it looked at the moment of rejection.
    ///
    /// Default: `Ok(())` — pass through unchanged.
    #[allow(unused_variables)]
    fn before(&self, input: &mut Op::Input) -> Result<(), VpnError> {
        Ok(())
    }

    /// Runs *after* the handler **succeeds**, in *reverse* registration order.
    ///
    /// Receives an immutable snapshot of the input (as it was when the handler
    /// ran) and `&mut output`. Modify output freely or return `Err` to replace
    /// the success with a failure — remaining `after` hooks are skipped and
    /// all `on_error` hooks fire.
    ///
    /// Default: `Ok(())` — pass through unchanged.
    #[allow(unused_variables)]
    fn after(&self, input: &Op::Input, output: &mut Op::Output) -> Result<(), VpnError> {
        Ok(())
    }

    /// Runs when the operation fails, in registration order.
    ///
    /// Receives an immutable snapshot of the input and the error. **Cannot
    /// modify either.** **Cannot suppress the error.** Use for logging,
    /// metrics, alerting, and cleanup side-effects.
    ///
    /// **All `on_error` hooks always run** — one hook's presence does not
    /// prevent others from seeing the error.
    ///
    /// Default: no-op.
    #[allow(unused_variables)]
    fn on_error(&self, input: &Op::Input, err: &VpnError) {}
}

// ---------------------------------------------------------------------------
// WrapHook — advanced escape hatch
// ---------------------------------------------------------------------------

/// Advanced escape hatch — Tower-style full wrapping with a `next` reference.
///
/// Use **only** when before/after/on_error cannot express your intent:
///
/// | Use case | Recommended |
/// |----------|-------------|
/// | Logging, tracing | `Hook::before` + `Hook::after` + `Hook::on_error` |
/// | Auth, validation, policy | `Hook::before` |
/// | Filtering / enriching output | `Hook::after` |
/// | Error notification, cleanup | `Hook::on_error` |
/// | **Retry with backoff** | **`WrapHook`** |
/// | **Response caching** | **`WrapHook`** |
/// | **Circuit breaker** | **`WrapHook`** |
/// | **Distributed tracing span** | **`WrapHook`** |
///
/// A `WrapHook` wraps the **entire** hook chain (all before/after/on_error hooks
/// and the handler). `next.handle(input)` runs everything inside.
/// First registered = outermost.
///
/// # Example — retry wrap
///
/// ```rust,ignore
/// struct RetryWrap { max: u32 }
/// impl WrapHook<OpConnect> for RetryWrap {
///     fn name(&self) -> &str { "wrap:retry" }
///     fn handle(&self, input: ConnectInput, next: &dyn Handler<OpConnect>)
///         -> Result<ConnectOutput, VpnError>
///     {
///         for attempt in 0..self.max {
///             match next.handle(input.clone()) {
///                 ok @ Ok(_) => return ok,
///                 Err(_) if attempt + 1 < self.max => continue,
///                 Err(e) => return Err(e),
///             }
///         }
///         unreachable!()
///     }
/// }
/// ```
pub trait WrapHook<Op: Operation>: Send + Sync {
    /// Unique name for diagnostics and deduplication.
    fn name(&self) -> &str;

    /// Execute with `next` pointing to the full inner hook chain + terminal handler.
    ///
    /// Call `next.handle(input)` to run everything inside this wrap hook.
    /// Because `Input: Clone`, you can call `next` multiple times (e.g. retry).
    fn handle(&self, input: Op::Input, next: &dyn Handler<Op>) -> Result<Op::Output, VpnError>;
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// A composed hook + wrap-hook chain with a terminal handler.
///
/// # Extension points
///
/// - [`push_hook`](Pipeline::push_hook) — registers a [`Hook`] for the standard
///   before/after/on_error lifecycle. Hooks run in registration order.
/// - [`push_wrap`](Pipeline::push_wrap) — registers a [`WrapHook`] that wraps
///   the entire hook chain. First registered = outermost.
///
/// # Execution model
///
/// ```text
/// pipeline.execute(input)
///  └─► WrapHook[0].handle(input, next → WrapHook[1])
///       └─► WrapHook[1].handle(input, next → HookRunner)
///            └─► HookRunner:
///                 ├─ Hook[0].before(&mut input)         ← can rewrite / reject
///                 ├─ Hook[1].before(&mut input)
///                 ├─ ... (abort on first Err)
///                 ├─ handler.handle(input)              ← snapshot taken here
///                 │
///                 ├─ [ok]  Hook[N].after(&snap, &mut out)  ← reverse order
///                 │         Hook[N-1].after(...)
///                 │
///                 └─ [err] Hook[0].on_error(&snap, &err)   ← forward order
///                           Hook[1].on_error(...)           ← all always run
/// ```
pub struct Pipeline<Op: Operation> {
    handler: Box<dyn Handler<Op>>,
    hooks: Vec<Box<dyn Hook<Op>>>,
    wraps: Vec<Box<dyn WrapHook<Op>>>,
}

impl<Op: Operation> Pipeline<Op> {
    /// Create a bare pipeline with only a terminal handler and no hooks.
    pub fn new(handler: Box<dyn Handler<Op>>) -> Self {
        Self {
            handler,
            hooks: Vec::new(),
            wraps: Vec::new(),
        }
    }

    /// Register a [`Hook`].
    ///
    /// Hooks run in registration order for `before` and `on_error`,
    /// and in *reverse* registration order for `after`.
    pub fn push_hook(&mut self, hook: Box<dyn Hook<Op>>) {
        self.hooks.push(hook);
    }

    /// Register a [`WrapHook`].
    ///
    /// First registered = outermost wrapper. Wraps the entire hook chain.
    pub fn push_wrap(&mut self, wrap: Box<dyn WrapHook<Op>>) {
        self.wraps.push(wrap);
    }

    /// Execute the pipeline with the given input.
    ///
    /// Returns `Err` if any `before` hook rejected, the handler failed,
    /// or any `after` hook rejected. `on_error` hooks always fire on failure
    /// but do not affect the returned error.
    pub fn execute(&self, input: Op::Input) -> Result<Op::Output, VpnError> {
        let runner = HookRunner {
            handler: &*self.handler,
            hooks: &self.hooks,
        };
        WrapCursor {
            wraps: &self.wraps,
            inner: &runner,
        }
        .handle(input)
    }

    /// Names of registered hooks, in registration (before/on_error execution) order.
    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(|h| h.name()).collect()
    }

    /// Names of registered wrap hooks, outermost first.
    pub fn wrap_names(&self) -> Vec<&str> {
        self.wraps.iter().map(|w| w.name()).collect()
    }

    /// Whether no hooks or wrap hooks are registered (pipeline is bare).
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty() && self.wraps.is_empty()
    }
}

// ---------------------------------------------------------------------------
// HookRunner — runs before / after / on_error hooks around the terminal handler
// ---------------------------------------------------------------------------

/// Internal: runs before/after/on_error hooks around the terminal handler.
///
/// This sits at the innermost position of the [`WrapCursor`] chain,
/// so every [`WrapHook`] in the pipeline wraps around it.
struct HookRunner<'p, Op: Operation> {
    handler: &'p dyn Handler<Op>,
    hooks: &'p [Box<dyn Hook<Op>>],
}

impl<Op: Operation> Handler<Op> for HookRunner<'_, Op> {
    fn handle(&self, mut input: Op::Input) -> Result<Op::Output, VpnError> {
        // ── Phase 1: before hooks (registration order) ──────────────────────
        //
        // Each hook receives &mut input so it can rewrite any field.
        // First Err short-circuits: remaining before-hooks are skipped,
        // on_error hooks fire, handler is never called.
        for hook in self.hooks.iter() {
            if let Err(e) = hook.before(&mut input) {
                for h in self.hooks.iter() {
                    h.on_error(&input, &e);
                }
                return Err(e);
            }
        }

        // Snapshot the (possibly-mutated) input before moving it into the handler.
        // `on_error` and `after` both receive this snapshot so they see the exact
        // field values that the handler saw.
        let snap = input.clone();

        // ── Phase 2: terminal handler ────────────────────────────────────────
        match self.handler.handle(input) {
            // ── Phase 3a: after hooks (reverse registration order) ───────────
            //
            // Runs only on success. Reverse order mirrors call-stack unwind:
            // the last hook to run `before` is the first to see the output.
            // Any Err here converts success → failure and fires on_error.
            Ok(mut output) => {
                for hook in self.hooks.iter().rev() {
                    if let Err(e) = hook.after(&snap, &mut output) {
                        for h in self.hooks.iter() {
                            h.on_error(&snap, &e);
                        }
                        return Err(e);
                    }
                }
                Ok(output)
            }

            // ── Phase 3b: on_error hooks (registration order, all run) ───────
            //
            // Runs only on failure. All hooks receive the error regardless of
            // what earlier on_error hooks do — they cannot short-circuit each other.
            Err(e) => {
                for hook in self.hooks.iter() {
                    hook.on_error(&snap, &e);
                }
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// WrapCursor — recursive walk through the WrapHook stack
// ---------------------------------------------------------------------------

/// Internal: recurses through [`WrapHook`]s, with [`HookRunner`] at the bottom.
struct WrapCursor<'p, Op: Operation> {
    wraps: &'p [Box<dyn WrapHook<Op>>],
    inner: &'p dyn Handler<Op>,
}

impl<Op: Operation> Handler<Op> for WrapCursor<'_, Op> {
    fn handle(&self, input: Op::Input) -> Result<Op::Output, VpnError> {
        match self.wraps.split_first() {
            // Head wrap gets `next` pointing to the rest of the stack.
            Some((head, tail)) => {
                let next = WrapCursor {
                    wraps: tail,
                    inner: self.inner,
                };
                head.handle(input, &next)
            }
            // No more wraps — delegate to the hook runner (or bare handler).
            None => self.inner.handle(input),
        }
    }
}

// ---------------------------------------------------------------------------
// Arc blanket impls — share hooks/wraps across multiple pipelines
// ---------------------------------------------------------------------------

/// Share a [`Hook`] across pipelines with `Arc` without cloning it.
impl<Op: Operation, H: Hook<Op>> Hook<Op> for std::sync::Arc<H> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn before(&self, input: &mut Op::Input) -> Result<(), VpnError> {
        (**self).before(input)
    }
    fn after(&self, input: &Op::Input, output: &mut Op::Output) -> Result<(), VpnError> {
        (**self).after(input, output)
    }
    fn on_error(&self, input: &Op::Input, err: &VpnError) {
        (**self).on_error(input, err)
    }
}

/// Share a [`WrapHook`] across pipelines with `Arc` without cloning it.
impl<Op: Operation, W: WrapHook<Op>> WrapHook<Op> for std::sync::Arc<W> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn handle(&self, input: Op::Input, next: &dyn Handler<Op>) -> Result<Op::Output, VpnError> {
        (**self).handle(input, next)
    }
}

// ---------------------------------------------------------------------------
// FnHook — closure-based hook for quick one-offs and tests
// ---------------------------------------------------------------------------

/// Closure-based [`Hook`] — build with a fluent API, register any subset of phases.
///
/// # Example
///
/// ```
/// use pingling_domain::pipeline::*;
/// use pingling_domain::VpnError;
///
/// # pub struct Greet;
/// # impl Operation for Greet {
/// #     type Input  = String;
/// #     type Output = String;
/// #     fn name() -> &'static str { "greet" }
/// # }
/// # struct Greeter;
/// # impl Handler<Greet> for Greeter {
/// #     fn handle(&self, n: String) -> Result<String, VpnError> { Ok(format!("Hello, {n}!")) }
/// # }
/// let mut pipeline = Pipeline::new(Box::new(Greeter));
/// pipeline.push_hook(Box::new(
///     FnHook::<Greet>::new("shout")
///         .after(|_input, output| { *output = output.to_uppercase(); Ok(()) })
///         .on_error(|_input, err| eprintln!("greet failed: {err}"))
/// ));
/// assert_eq!(pipeline.execute("world".into()).unwrap(), "HELLO, WORLD!");
/// ```
pub struct FnHook<Op: Operation> {
    label: &'static str,
    before_fn: Option<Box<dyn Fn(&mut Op::Input) -> Result<(), VpnError> + Send + Sync>>,
    after_fn:
        Option<Box<dyn Fn(&Op::Input, &mut Op::Output) -> Result<(), VpnError> + Send + Sync>>,
    on_error_fn: Option<Box<dyn Fn(&Op::Input, &VpnError) + Send + Sync>>,
}

impl<Op: Operation> FnHook<Op> {
    /// Create a named hook with no phases active yet.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            before_fn: None,
            after_fn: None,
            on_error_fn: None,
        }
    }

    /// Register a closure for the `before` phase.
    pub fn before<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut Op::Input) -> Result<(), VpnError> + Send + Sync + 'static,
    {
        self.before_fn = Some(Box::new(f));
        self
    }

    /// Register a closure for the `after` phase.
    pub fn after<F>(mut self, f: F) -> Self
    where
        F: Fn(&Op::Input, &mut Op::Output) -> Result<(), VpnError> + Send + Sync + 'static,
    {
        self.after_fn = Some(Box::new(f));
        self
    }

    /// Register a closure for the `on_error` phase.
    pub fn on_error<F>(mut self, f: F) -> Self
    where
        F: Fn(&Op::Input, &VpnError) + Send + Sync + 'static,
    {
        self.on_error_fn = Some(Box::new(f));
        self
    }
}

impl<Op: Operation> Hook<Op> for FnHook<Op> {
    fn name(&self) -> &str {
        self.label
    }

    fn before(&self, input: &mut Op::Input) -> Result<(), VpnError> {
        if let Some(f) = &self.before_fn {
            f(input)
        } else {
            Ok(())
        }
    }

    fn after(&self, input: &Op::Input, output: &mut Op::Output) -> Result<(), VpnError> {
        if let Some(f) = &self.after_fn {
            f(input, output)
        } else {
            Ok(())
        }
    }

    fn on_error(&self, input: &Op::Input, err: &VpnError) {
        if let Some(f) = &self.on_error_fn {
            f(input, err);
        }
    }
}

// ---------------------------------------------------------------------------
// FnWrapHook — closure-based WrapHook for quick one-offs and tests
// ---------------------------------------------------------------------------

/// Closure-based [`WrapHook`] — turn a closure into a full Tower-style wrapper.
///
/// # Example — block all requests
///
/// ```
/// use pingling_domain::pipeline::*;
/// use pingling_domain::VpnError;
///
/// # pub struct Greet;
/// # impl Operation for Greet {
/// #     type Input  = String;
/// #     type Output = String;
/// #     fn name() -> &'static str { "greet" }
/// # }
/// # struct Greeter;
/// # impl Handler<Greet> for Greeter {
/// #     fn handle(&self, n: String) -> Result<String, VpnError> { Ok(format!("Hello, {n}!")) }
/// # }
/// let mut pipeline = Pipeline::new(Box::new(Greeter));
/// pipeline.push_wrap(Box::new(FnWrapHook::<Greet, _>::new("block", |_input, _next| {
///     Err(VpnError::Unknown("blocked by policy".into()))
/// })));
/// assert!(pipeline.execute("world".into()).is_err());
/// ```
pub struct FnWrapHook<Op: Operation, F>
where
    F: Fn(Op::Input, &dyn Handler<Op>) -> Result<Op::Output, VpnError> + Send + Sync,
{
    label: &'static str,
    f: F,
    _op: std::marker::PhantomData<Op>,
}

impl<Op: Operation, F> FnWrapHook<Op, F>
where
    F: Fn(Op::Input, &dyn Handler<Op>) -> Result<Op::Output, VpnError> + Send + Sync,
{
    pub fn new(label: &'static str, f: F) -> Self {
        Self {
            label,
            f,
            _op: std::marker::PhantomData,
        }
    }
}

impl<Op: Operation, F> WrapHook<Op> for FnWrapHook<Op, F>
where
    F: Fn(Op::Input, &dyn Handler<Op>) -> Result<Op::Output, VpnError> + Send + Sync,
{
    fn name(&self) -> &str {
        self.label
    }

    fn handle(&self, input: Op::Input, next: &dyn Handler<Op>) -> Result<Op::Output, VpnError> {
        (self.f)(input, next)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    // ── Test operation: add two numbers ─────────────────────────────────────

    struct OpAdd;
    impl Operation for OpAdd {
        type Input = (i32, i32);
        type Output = i32;
        fn name() -> &'static str {
            "add"
        }
    }

    struct AddHandler;
    impl Handler<OpAdd> for AddHandler {
        fn handle(&self, (a, b): (i32, i32)) -> Result<i32, VpnError> {
            Ok(a + b)
        }
    }

    struct FailHandler;
    impl Handler<OpAdd> for FailHandler {
        fn handle(&self, _: (i32, i32)) -> Result<i32, VpnError> {
            Err(VpnError::Unknown("handler failed".into()))
        }
    }

    // ── Bare pipeline ────────────────────────────────────────────────────────

    #[test]
    fn bare_pipeline_calls_handler() {
        let p = Pipeline::<OpAdd>::new(Box::new(AddHandler));
        assert_eq!(p.execute((3, 4)).unwrap(), 7);
        assert!(p.is_empty());
    }

    // ── Hook: before modifies input ──────────────────────────────────────────

    #[test]
    fn before_hook_can_modify_input() {
        // Doubles both operands before the add
        struct DoubleInputHook;
        impl Hook<OpAdd> for DoubleInputHook {
            fn name(&self) -> &str {
                "double-input"
            }
            fn before(&self, input: &mut (i32, i32)) -> Result<(), VpnError> {
                input.0 *= 2;
                input.1 *= 2;
                Ok(())
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(DoubleInputHook));
        // (3, 4) → before doubles to (6, 8) → handler: 6 + 8 = 14
        assert_eq!(p.execute((3, 4)).unwrap(), 14);
    }

    // ── Hook: after modifies output ──────────────────────────────────────────

    #[test]
    fn after_hook_can_modify_output() {
        struct NegateOutputHook;
        impl Hook<OpAdd> for NegateOutputHook {
            fn name(&self) -> &str {
                "negate"
            }
            fn after(&self, _input: &(i32, i32), output: &mut i32) -> Result<(), VpnError> {
                *output = -*output;
                Ok(())
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(NegateOutputHook));
        // 3 + 4 = 7 → after negates to -7
        assert_eq!(p.execute((3, 4)).unwrap(), -7);
    }

    // ── Hook: before rejection short-circuits handler ────────────────────────

    #[test]
    fn before_rejection_skips_handler() {
        let handler_called = Arc::new(AtomicBool::new(false));

        struct CheckingHandler(Arc<AtomicBool>);
        impl Handler<OpAdd> for CheckingHandler {
            fn handle(&self, _: (i32, i32)) -> Result<i32, VpnError> {
                self.0.store(true, Ordering::SeqCst);
                Ok(0)
            }
        }

        struct RejectHook;
        impl Hook<OpAdd> for RejectHook {
            fn name(&self) -> &str {
                "reject"
            }
            fn before(&self, _: &mut (i32, i32)) -> Result<(), VpnError> {
                Err(VpnError::Unknown("policy denied".into()))
            }
        }

        let mut p = Pipeline::new(Box::new(CheckingHandler(handler_called.clone())));
        p.push_hook(Box::new(RejectHook));

        let result = p.execute((1, 2));
        assert!(result.is_err());
        assert!(
            !handler_called.load(Ordering::SeqCst),
            "handler must not run"
        );
    }

    // ── Hook: before rejection fires on_error ────────────────────────────────

    #[test]
    fn before_rejection_fires_on_error() {
        let error_seen = Arc::new(AtomicBool::new(false));

        struct RejectHook;
        impl Hook<OpAdd> for RejectHook {
            fn name(&self) -> &str {
                "reject"
            }
            fn before(&self, _: &mut (i32, i32)) -> Result<(), VpnError> {
                Err(VpnError::Unknown("gate".into()))
            }
        }

        struct ObserveHook(Arc<AtomicBool>);
        impl Hook<OpAdd> for ObserveHook {
            fn name(&self) -> &str {
                "observe"
            }
            fn on_error(&self, _input: &(i32, i32), _err: &VpnError) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(RejectHook));
        p.push_hook(Box::new(ObserveHook(error_seen.clone())));

        assert!(p.execute((1, 2)).is_err());
        assert!(error_seen.load(Ordering::SeqCst), "on_error must fire");
    }

    // ── Hook: after rejection fires on_error and skips remaining afters ──────

    #[test]
    fn after_rejection_fires_on_error_skips_remaining_afters() {
        // Hook A (registered first): after rejects
        // Hook B (registered second): after should NOT run; on_error should run
        let b_after_called = Arc::new(AtomicBool::new(false));
        let b_on_error_called = Arc::new(AtomicBool::new(false));

        struct RejectAfterHook;
        impl Hook<OpAdd> for RejectAfterHook {
            fn name(&self) -> &str {
                "reject-after"
            }
            fn after(&self, _: &(i32, i32), _: &mut i32) -> Result<(), VpnError> {
                Err(VpnError::Unknown("bad output".into()))
            }
        }

        struct ObserveHook {
            after_called: Arc<AtomicBool>,
            on_error_called: Arc<AtomicBool>,
        }
        impl Hook<OpAdd> for ObserveHook {
            fn name(&self) -> &str {
                "observe-b"
            }
            fn after(&self, _: &(i32, i32), _: &mut i32) -> Result<(), VpnError> {
                self.after_called.store(true, Ordering::SeqCst);
                Ok(())
            }
            fn on_error(&self, _: &(i32, i32), _: &VpnError) {
                self.on_error_called.store(true, Ordering::SeqCst);
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        // Registration order: B first, then A.
        // after runs in REVERSE order: A runs first (and rejects), B is skipped.
        p.push_hook(Box::new(ObserveHook {
            after_called: b_after_called.clone(),
            on_error_called: b_on_error_called.clone(),
        }));
        p.push_hook(Box::new(RejectAfterHook));

        let result = p.execute((3, 4));
        assert!(result.is_err());
        // B's after was supposed to run before A (LIFO), but A ran first and failed.
        // B's after is skipped.
        assert!(
            !b_after_called.load(Ordering::SeqCst),
            "B.after must be skipped"
        );
        // B's on_error must still fire.
        assert!(
            b_on_error_called.load(Ordering::SeqCst),
            "B.on_error must fire"
        );
    }

    // ── Hook: handler failure fires on_error ─────────────────────────────────

    #[test]
    fn handler_failure_fires_on_error() {
        let seen = Arc::new(AtomicBool::new(false));

        struct Observer(Arc<AtomicBool>);
        impl Hook<OpAdd> for Observer {
            fn name(&self) -> &str {
                "obs"
            }
            fn on_error(&self, _: &(i32, i32), _: &VpnError) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let mut p = Pipeline::<OpAdd>::new(Box::new(FailHandler));
        p.push_hook(Box::new(Observer(seen.clone())));

        assert!(p.execute((1, 2)).is_err());
        assert!(seen.load(Ordering::SeqCst));
    }

    // ── Hook: all on_error hooks always run (even when multiple exist) ────────

    #[test]
    fn all_on_error_hooks_always_run() {
        let count = Arc::new(AtomicU32::new(0));

        struct CountOnError(Arc<AtomicU32>);
        impl Hook<OpAdd> for CountOnError {
            fn name(&self) -> &str {
                "count"
            }
            fn on_error(&self, _: &(i32, i32), _: &VpnError) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let mut p = Pipeline::<OpAdd>::new(Box::new(FailHandler));
        p.push_hook(Box::new(CountOnError(count.clone())));
        p.push_hook(Box::new(CountOnError(count.clone())));
        p.push_hook(Box::new(CountOnError(count.clone())));

        assert!(p.execute((0, 0)).is_err());
        // All three on_error hooks must have run.
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    // ── Hook: before/after ordering is registration order / reverse ──────────

    #[test]
    fn before_registration_order_after_reverse_order() {
        // Record the sequence of hook executions via a shared log.
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        use std::sync::Mutex;

        struct SeqHook {
            id: &'static str,
            log: Arc<Mutex<Vec<&'static str>>>,
        }
        impl Hook<OpAdd> for SeqHook {
            fn name(&self) -> &str {
                self.id
            }
            fn before(&self, _: &mut (i32, i32)) -> Result<(), VpnError> {
                self.log.lock().unwrap().push(self.id);
                Ok(())
            }
            fn after(&self, _: &(i32, i32), _: &mut i32) -> Result<(), VpnError> {
                // Use a different tag to distinguish before vs after
                let tag: &'static str = match self.id {
                    "A" => "A-after",
                    "B" => "B-after",
                    "C" => "C-after",
                    _ => "?",
                };
                self.log.lock().unwrap().push(tag);
                Ok(())
            }
        }

        let make = |id| SeqHook {
            id,
            log: log.clone(),
        };

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(make("A"))); // registered first
        p.push_hook(Box::new(make("B")));
        p.push_hook(Box::new(make("C"))); // registered last

        p.execute((1, 1)).unwrap();

        let seq = log.lock().unwrap().clone();
        // before: A → B → C (registration order)
        // after:  C-after → B-after → A-after (reverse)
        assert_eq!(seq, vec!["A", "B", "C", "C-after", "B-after", "A-after"]);
    }

    // ── Hook: on_error receives the input snapshot (as it was after befores) ─

    #[test]
    fn on_error_receives_post_before_input_snapshot() {
        // A before hook rewrites input. The handler then fails.
        // on_error must see the rewritten input, not the original.
        let seen_value = Arc::new(AtomicU32::new(0));

        struct RewriteHook;
        impl Hook<OpAdd> for RewriteHook {
            fn name(&self) -> &str {
                "rewrite"
            }
            fn before(&self, input: &mut (i32, i32)) -> Result<(), VpnError> {
                input.0 = 99; // rewrite
                Ok(())
            }
        }

        struct SnapshotObserver(Arc<AtomicU32>);
        impl Hook<OpAdd> for SnapshotObserver {
            fn name(&self) -> &str {
                "snapshot-obs"
            }
            fn on_error(&self, input: &(i32, i32), _: &VpnError) {
                self.0.store(input.0 as u32, Ordering::SeqCst);
            }
        }

        let mut p = Pipeline::<OpAdd>::new(Box::new(FailHandler));
        p.push_hook(Box::new(RewriteHook));
        p.push_hook(Box::new(SnapshotObserver(seen_value.clone())));

        assert!(p.execute((0, 0)).is_err());
        // on_error must see 99, the value after RewriteHook ran.
        assert_eq!(seen_value.load(Ordering::SeqCst), 99);
    }

    // ── WrapHook: wraps the entire hook chain ─────────────────────────────────

    #[test]
    fn wrap_hook_wraps_entire_chain() {
        let before_ran = Arc::new(AtomicBool::new(false));
        let after_ran = Arc::new(AtomicBool::new(false));

        struct TrackHook {
            before: Arc<AtomicBool>,
            after: Arc<AtomicBool>,
        }
        impl Hook<OpAdd> for TrackHook {
            fn name(&self) -> &str {
                "track"
            }
            fn before(&self, _: &mut (i32, i32)) -> Result<(), VpnError> {
                self.before.store(true, Ordering::SeqCst);
                Ok(())
            }
            fn after(&self, _: &(i32, i32), _: &mut i32) -> Result<(), VpnError> {
                self.after.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        // WrapHook doubles the output of whatever `next` returns.
        struct DoubleWrap;
        impl WrapHook<OpAdd> for DoubleWrap {
            fn name(&self) -> &str {
                "double-wrap"
            }
            fn handle(
                &self,
                input: (i32, i32),
                next: &dyn Handler<OpAdd>,
            ) -> Result<i32, VpnError> {
                let result = next.handle(input)?;
                Ok(result * 2)
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(TrackHook {
            before: before_ran.clone(),
            after: after_ran.clone(),
        }));
        p.push_wrap(Box::new(DoubleWrap));

        // 3+4=7, doubled by wrap = 14. Hooks still fire inside the wrap.
        assert_eq!(p.execute((3, 4)).unwrap(), 14);
        assert!(
            before_ran.load(Ordering::SeqCst),
            "before must run inside wrap"
        );
        assert!(
            after_ran.load(Ordering::SeqCst),
            "after must run inside wrap"
        );
    }

    // ── WrapHook: retry pattern (calls next multiple times) ───────────────────

    #[test]
    fn wrap_hook_can_retry() {
        let attempt_count = Arc::new(AtomicU32::new(0));

        struct CountingHandler(Arc<AtomicU32>);
        impl Handler<OpAdd> for CountingHandler {
            fn handle(&self, (a, b): (i32, i32)) -> Result<i32, VpnError> {
                let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
                // Fail on first two attempts, succeed on third.
                if n < 3 {
                    Err(VpnError::Unknown("transient".into()))
                } else {
                    Ok(a + b)
                }
            }
        }

        struct RetryWrap {
            max: u32,
        }
        impl WrapHook<OpAdd> for RetryWrap {
            fn name(&self) -> &str {
                "retry"
            }
            fn handle(
                &self,
                input: (i32, i32),
                next: &dyn Handler<OpAdd>,
            ) -> Result<i32, VpnError> {
                for attempt in 0..self.max {
                    match next.handle(input) {
                        ok @ Ok(_) => return ok,
                        Err(e) if attempt + 1 < self.max => {
                            let _ = e;
                        }
                        Err(e) => return Err(e),
                    }
                }
                unreachable!()
            }
        }

        let mut p = Pipeline::new(Box::new(CountingHandler(attempt_count.clone())));
        p.push_wrap(Box::new(RetryWrap { max: 5 }));

        assert_eq!(p.execute((10, 5)).unwrap(), 15);
        assert_eq!(attempt_count.load(Ordering::SeqCst), 3);
    }

    // ── WrapHook: caching — skips inner chain on cache hit ────────────────────

    #[test]
    fn wrap_hook_can_cache_and_skip_handler() {
        let handler_calls = Arc::new(AtomicU32::new(0));

        struct CountHandler(Arc<AtomicU32>);
        impl Handler<OpAdd> for CountHandler {
            fn handle(&self, (a, b): (i32, i32)) -> Result<i32, VpnError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(a + b)
            }
        }

        struct OnceCache {
            cached: std::sync::Mutex<Option<i32>>,
        }
        impl WrapHook<OpAdd> for OnceCache {
            fn name(&self) -> &str {
                "cache"
            }
            fn handle(
                &self,
                input: (i32, i32),
                next: &dyn Handler<OpAdd>,
            ) -> Result<i32, VpnError> {
                let mut guard = self.cached.lock().unwrap();
                if let Some(v) = *guard {
                    return Ok(v);
                }
                let v = next.handle(input)?;
                *guard = Some(v);
                Ok(v)
            }
        }

        let mut p = Pipeline::new(Box::new(CountHandler(handler_calls.clone())));
        p.push_wrap(Box::new(OnceCache {
            cached: std::sync::Mutex::new(None),
        }));

        assert_eq!(p.execute((2, 3)).unwrap(), 5);
        assert_eq!(p.execute((2, 3)).unwrap(), 5); // cache hit
        assert_eq!(handler_calls.load(Ordering::SeqCst), 1); // handler ran only once
    }

    // ── WrapHook: outermost first ─────────────────────────────────────────────

    #[test]
    fn wrap_hook_first_registered_is_outermost() {
        // Each wrap multiplies by a different factor.
        // Outermost = last to apply. If order is A(x2) then B(x3):
        // result = (3+4) * 3 * 2 = 42  (B inner, A outer)
        struct MulWrap(i32);
        impl WrapHook<OpAdd> for MulWrap {
            fn name(&self) -> &str {
                "mul"
            }
            fn handle(
                &self,
                input: (i32, i32),
                next: &dyn Handler<OpAdd>,
            ) -> Result<i32, VpnError> {
                Ok(next.handle(input)? * self.0)
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_wrap(Box::new(MulWrap(2))); // outermost: multiplies last
        p.push_wrap(Box::new(MulWrap(3))); // innermost: multiplies first

        // (3+4)=7 → MulWrap(3) → 21 → MulWrap(2) → 42
        assert_eq!(p.execute((3, 4)).unwrap(), 42);
    }

    // ── Arc sharing ───────────────────────────────────────────────────────────

    #[test]
    fn arc_hook_can_be_shared_across_pipelines() {
        struct SharedHook;
        impl Hook<OpAdd> for SharedHook {
            fn name(&self) -> &str {
                "shared"
            }
            fn after(&self, _: &(i32, i32), output: &mut i32) -> Result<(), VpnError> {
                *output += 100;
                Ok(())
            }
        }

        let shared = Arc::new(SharedHook);

        let mut p1 = Pipeline::new(Box::new(AddHandler));
        p1.push_hook(Box::new(Arc::clone(&shared)));

        let mut p2 = Pipeline::new(Box::new(AddHandler));
        p2.push_hook(Box::new(Arc::clone(&shared)));

        assert_eq!(p1.execute((1, 2)).unwrap(), 103); // 3 + 100
        assert_eq!(p2.execute((4, 5)).unwrap(), 109); // 9 + 100
    }

    // ── FnHook fluent API ─────────────────────────────────────────────────────

    #[test]
    fn fn_hook_fluent_api() {
        let error_logged = Arc::new(AtomicBool::new(false));
        let err_flag = error_logged.clone();

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(
            FnHook::<OpAdd>::new("fn-hook")
                .before(|input| {
                    input.0 *= 10; // scale first operand
                    Ok(())
                })
                .after(|_, output| {
                    *output -= 1; // subtract one from result
                    Ok(())
                })
                .on_error(move |_, _| {
                    err_flag.store(true, Ordering::SeqCst);
                }),
        ));

        // before: (2,3) → (20,3). handler: 20+3=23. after: 23-1=22.
        assert_eq!(p.execute((2, 3)).unwrap(), 22);
        assert!(!error_logged.load(Ordering::SeqCst));
    }

    // ── FnWrapHook convenience ────────────────────────────────────────────────

    #[test]
    fn fn_wrap_hook_can_rewrite_input() {
        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_wrap(Box::new(FnWrapHook::<OpAdd, _>::new(
            "rewrite-wrap",
            |_input, next| next.handle((100, 200)),
        )));
        // Input is ignored; wrap forces (100, 200) → 300.
        assert_eq!(p.execute((1, 2)).unwrap(), 300);
    }

    // ── Pipeline introspection ────────────────────────────────────────────────

    #[test]
    fn pipeline_introspection() {
        struct H1;
        impl Hook<OpAdd> for H1 {
            fn name(&self) -> &str {
                "hook-a"
            }
        }
        struct H2;
        impl Hook<OpAdd> for H2 {
            fn name(&self) -> &str {
                "hook-b"
            }
        }
        struct W1;
        impl WrapHook<OpAdd> for W1 {
            fn name(&self) -> &str {
                "wrap-x"
            }
            fn handle(&self, i: (i32, i32), n: &dyn Handler<OpAdd>) -> Result<i32, VpnError> {
                n.handle(i)
            }
        }

        let mut p = Pipeline::new(Box::new(AddHandler));
        assert!(p.is_empty());

        p.push_hook(Box::new(H1));
        p.push_hook(Box::new(H2));
        p.push_wrap(Box::new(W1));

        assert!(!p.is_empty());
        assert_eq!(p.hook_names(), vec!["hook-a", "hook-b"]);
        assert_eq!(p.wrap_names(), vec!["wrap-x"]);
    }

    // ── Pipeline is Send + Sync ───────────────────────────────────────────────

    #[test]
    fn pipeline_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Pipeline<OpAdd>>();
    }

    // ── Operation name is accessible statically ───────────────────────────────

    #[test]
    fn operation_name() {
        assert_eq!(OpAdd::name(), "add");
    }

    // ── Mixed: hooks + wraps compose correctly ────────────────────────────────

    #[test]
    fn hooks_and_wraps_compose_correctly() {
        // before: add 1 to each operand
        // handler: add
        // after: multiply output by 2
        // wrap: subtract 1 from final result
        //
        // (3,4) → before: (4,5) → handler: 9 → after: 18 → wrap: 17

        let mut p = Pipeline::new(Box::new(AddHandler));
        p.push_hook(Box::new(
            FnHook::<OpAdd>::new("add1-before")
                .before(|input| {
                    input.0 += 1;
                    input.1 += 1;
                    Ok(())
                })
                .after(|_, out| {
                    *out *= 2;
                    Ok(())
                }),
        ));
        p.push_wrap(Box::new(FnWrapHook::<OpAdd, _>::new(
            "sub1-wrap",
            |input, next| Ok(next.handle(input)? - 1),
        )));

        assert_eq!(p.execute((3, 4)).unwrap(), 17);
    }
}
