//! Internal proc-macros for the cqs crate.
//!
//! `#[derive(CqsCommands)]` generates the command registry from the
//! `Commands` enum. Per-variant attributes live on the enum; the macro emits:
//!
//!   * `Commands::variant_name(&self) -> &'static str`
//!   * `Commands::batch_support(&self) -> BatchSupport`
//!   * `pub fn dispatch_group_a(cli: &Cli, project_cqs_dir: &Path) -> std::ops::ControlFlow<Result<()>, ()>`
//!     — runs every Group A variant's dispatch shim; returns
//!     `ControlFlow::Break(result)` when handled, `Continue(())` when the
//!     command is a Group B variant.
//!   * `pub fn dispatch_group_b(cli: &Cli, ctx: &CommandContext<ReadOnly>, project_cqs_dir: &Path) -> Result<()>`
//!     — runs every Group B variant's dispatch shim; bare-query and Group A
//!     variants land in the catch-all that prints the usage banner.
//!
//! Per-variant attribute syntax:
//!
//! ```text
//! #[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]
//! Foo { ... }
//! ```
//!
//! Variant telemetry name auto-derives from the variant ident as kebab-case
//! (matches clap's default subcommand name): `Init` → `"init"`, `TrainData` →
//! `"train-data"`, `AuditMode` → `"audit-mode"`. Override with
//! `#[cqs_cmd(name = "...")]` only if the kebab default doesn't match the
//! existing telemetry label. The attribute is named `cqs_cmd` rather than
//! `command` so it doesn't collide with clap's `#[command(...)]` namespace
//! on the same enum.
//!
//! `batch = "runtime"` defers to a function named `<variant_snake>_batch_support`
//! taking `&Commands` and returning `BatchSupport` — used by `Notes` and
//! `Suggest` whose support level depends on the inner subcommand.
//!
//! Each Group A or Group B variant must have a sibling function named
//! `cmd_<variant_snake>_dispatch` with the standardized signature:
//!
//! ```text
//! pub fn cmd_foo_dispatch(
//!     cli: &Cli,
//!     ctx: Option<&CommandContext<'_, ReadOnly>>,
//!     project_cqs_dir: &Path,
//!     cmd: &Commands,
//! ) -> Result<()>;
//! ```
//!
//! The shim destructures `cmd` to recover the variant fields and calls
//! the actual handler. This keeps handler signatures uniform while the
//! body-bodies live where the implementation actually is.
//!
//! Cfg-gated variants (`#[cfg(feature = "...")]`) are forwarded verbatim
//! to every emitted arm so the variant disappears consistently across
//! all four generated functions.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Ident, Variant};

#[proc_macro_derive(CqsCommands, attributes(cqs_cmd))]
pub fn derive_cqs_commands(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let enum_ident = &input.ident;

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "CqsCommands can only be derived on enums",
            ))
        }
    };

    let mut parsed: Vec<ParsedVariant> = Vec::new();
    for variant in variants {
        parsed.push(ParsedVariant::from_variant(variant)?);
    }

    let variant_name_arms = parsed.iter().map(|v| v.variant_name_arm(enum_ident));
    let batch_support_arms = parsed.iter().map(|v| v.batch_support_arm(enum_ident));

    // Names of variants that can route to the daemon's batch dispatcher:
    // `batch = "daemon"` always, plus `batch = "runtime"` (whose support is
    // decided per-invocation but resolves to Daemon for some inner
    // subcommands). Emitted as `daemon_capable_variant_names()` so a test can
    // assert every name has a matching `BatchCmd` subcommand — without this,
    // the link between the two enums is checked only at runtime, only
    // daemon-up.
    let daemon_capable_pushes = parsed
        .iter()
        .filter(|v| {
            matches!(
                v.batch,
                BatchSupportKind::Daemon | BatchSupportKind::Runtime
            )
        })
        .map(|v| {
            let cfg = &v.cfg_attrs;
            let name = &v.name_lit;
            quote! {
                #(#cfg)*
                names.push(#name);
            }
        });

    let group_a_dispatch_arms = parsed
        .iter()
        .filter(|v| matches!(v.group, Group::A))
        .map(|v| v.group_a_dispatch_arm(enum_ident));
    let group_b_dispatch_arms = parsed
        .iter()
        .filter(|v| matches!(v.group, Group::B))
        .map(|v| v.group_b_dispatch_arm(enum_ident));

    // Group B variants reachable from the Group A match must land in a
    // wildcard arm (we'll handle them after ctx is open). Likewise Group A
    // variants reachable from the Group B match unreachable!(). Per-variant
    // wildcards keep cfg-gating clean — the simpler `_ => {}` catch-all
    // would defeat the exhaustiveness guarantee that makes a missing
    // `#[command(...)]` attribute a compile error.
    let group_a_passthrough = parsed
        .iter()
        .filter(|v| matches!(v.group, Group::B))
        .map(|v| v.passthrough_arm(enum_ident, /*group_b_seen_in_a*/ true));
    let group_b_unreachable = parsed
        .iter()
        .filter(|v| matches!(v.group, Group::A))
        .map(|v| v.passthrough_arm(enum_ident, /*group_b_seen_in_a*/ false));

    // Emit a const-eval shim-existence guard. If any
    // `cmd_<snake>_dispatch` function is missing, the error fires once
    // here with a clear "function not found in `crate::cli::commands`"
    // message, rather than scattered per-call-site errors inside the
    // generated match arms. The const block is `#[allow(unused)]` and
    // expanded at compile time — zero runtime cost.
    let dispatch_existence_checks = parsed.iter().map(|v| v.shim_existence_check());

    Ok(quote! {
        // Consolidated shim-existence check. If any
        // `cmd_<snake>_dispatch` function is missing or has the wrong
        // signature, the error fires here with a single clear message
        // tied to the derive expansion. Without this block, you'd get
        // ~58 scattered "function not found" errors at every match arm
        // call site.
        //
        // `const _: ()` is a compile-time-only assertion: the function
        // pointers are coerced through a typed local binding, which
        // forces both name resolution and signature compatibility, then
        // discarded. No runtime cost.
        #[allow(unused, clippy::no_effect_underscore_binding)]
        const _: () = {
            type DispatchFn = fn(
                &crate::cli::definitions::Cli,
                ::std::option::Option<&crate::cli::CommandContext<'_, ::cqs::store::ReadOnly>>,
                &::std::path::Path,
                &#enum_ident,
            ) -> ::anyhow::Result<()>;
            #(#dispatch_existence_checks)*
        };

        impl #enum_ident {
            /// Stable telemetry label for this variant. Must stay in lock-step
            /// with the `#[command(name = "...")]` attribute on every variant.
            pub fn variant_name(&self) -> &'static str {
                match self {
                    #(#variant_name_arms,)*
                }
            }

            /// Whether this variant can be answered by the daemon
            /// (`BatchSupport::Daemon`) or must run in-process
            /// (`BatchSupport::Cli`). Variants whose support level depends on
            /// the inner subcommand declare `batch = "runtime"` and define a
            /// `<variant_snake>_batch_support(&Commands) -> BatchSupport`
            /// helper.
            pub fn batch_support(&self) -> crate::cli::BatchSupport {
                match self {
                    #(#batch_support_arms,)*
                }
            }

            /// Wire-level names of every variant that may forward to the
            /// daemon's batch dispatcher (`batch = "daemon"` plus
            /// `batch = "runtime"`). A test asserts each name resolves to a
            /// `BatchCmd` subcommand, so a variant marked daemon-capable
            /// without a batch handler fails at test time instead of at
            /// runtime, daemon-up only.
            #[allow(
                dead_code,
                reason = "consumed by the exhaustiveness test in cli::batch::commands"
            )]
            pub fn daemon_capable_variant_names() -> Vec<&'static str> {
                let mut names = Vec::new();
                #(#daemon_capable_pushes)*
                names
            }
        }

        /// Dispatch every Group A (no-store / mutation) variant. Returns
        /// `ControlFlow::Break(Ok(()))` when handled, or `Continue(())` when
        /// `cli.command` is a Group B variant or `None` (caller opens the
        /// store and falls through to `dispatch_group_b`).
        ///
        /// The `cli` arg is owned-passed so handlers can take `&Cli` for
        /// the entire command lifetime.
        pub fn dispatch_group_a(
            cli: &crate::cli::definitions::Cli,
            project_cqs_dir: &std::path::Path,
        ) -> std::ops::ControlFlow<anyhow::Result<()>, ()> {
            match cli.command.as_ref() {
                #(#group_a_dispatch_arms,)*
                #(#group_a_passthrough,)*
                None => std::ops::ControlFlow::Continue(()),
            }
        }

        /// Dispatch every Group B (store-using) variant. Bare-query and
        /// Group A variants land in the wildcard branches: bare-query falls
        /// to `cmd_query`, Group A variants `unreachable!()` (caller must
        /// have handled them via `dispatch_group_a`).
        pub fn dispatch_group_b<'a>(
            cli: &'a crate::cli::definitions::Cli,
            ctx: &'a crate::cli::CommandContext<'a, ::cqs::store::ReadOnly>,
            project_cqs_dir: &'a std::path::Path,
        ) -> anyhow::Result<()> {
            match cli.command.as_ref() {
                #(#group_b_dispatch_arms,)*
                #(#group_b_unreachable,)*
                None => match cli.query.as_deref() {
                    Some(q) => crate::cli::commands::cmd_query(ctx, q),
                    None => {
                        println!("Usage: cqs <query> or cqs <command>");
                        println!("Run 'cqs --help' for more information.");
                        Ok(())
                    }
                },
            }
        }
    })
}

#[derive(Debug, Clone, Copy)]
enum Group {
    A,
    B,
}

#[derive(Debug, Clone, Copy)]
enum BatchSupportKind {
    Cli,
    Daemon,
    Runtime,
}

struct ParsedVariant {
    ident: Ident,
    name_lit: String,
    group: Group,
    batch: BatchSupportKind,
    is_unit: bool,
    cfg_attrs: Vec<syn::Attribute>,
}

impl ParsedVariant {
    fn from_variant(variant: &Variant) -> syn::Result<Self> {
        let ident = variant.ident.clone();
        let is_unit = matches!(variant.fields, Fields::Unit);

        let mut name_lit: Option<String> = None;
        let mut group: Option<Group> = None;
        let mut batch: Option<BatchSupportKind> = None;

        for attr in &variant.attrs {
            if !attr.path().is_ident("cqs_cmd") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    name_lit = Some(value.value());
                } else if meta.path.is_ident("group") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    match value.value().as_str() {
                        "a" | "A" => group = Some(Group::A),
                        "b" | "B" => group = Some(Group::B),
                        other => {
                            return Err(
                                meta.error(format!("expected `\"a\"` or `\"b\"`, got {other:?}"))
                            )
                        }
                    }
                } else if meta.path.is_ident("batch") {
                    let value: syn::LitStr = meta.value()?.parse()?;
                    batch = Some(match value.value().as_str() {
                        "cli" | "Cli" => BatchSupportKind::Cli,
                        "daemon" | "Daemon" => BatchSupportKind::Daemon,
                        "runtime" => BatchSupportKind::Runtime,
                        other => {
                            return Err(meta.error(format!(
                                "expected `\"cli\"`, `\"daemon\"`, or `\"runtime\"`, got {other:?}"
                            )))
                        }
                    });
                }
                Ok(())
            })?;
        }

        // Default name: kebab-case of variant ident (matches clap's default
        // subcommand name). `Init` → `"init"`, `TrainData` → `"train-data"`,
        // `AuditMode` → `"audit-mode"`. Override only when the telemetry
        // label diverges from the kebab-case default.
        let name_lit = name_lit.unwrap_or_else(|| to_kebab_case(&variant.ident.to_string()));
        let group = group.ok_or_else(|| {
            syn::Error::new_spanned(
                &variant.ident,
                "missing `#[cqs_cmd(group = \"a\"|\"b\")]` attribute",
            )
        })?;
        let batch = batch.ok_or_else(|| {
            syn::Error::new_spanned(
                &variant.ident,
                "missing `#[cqs_cmd(batch = \"cli\"|\"daemon\"|\"runtime\")]` attribute",
            )
        })?;

        // Forward only `#[cfg(...)]` attributes — every other attribute
        // (#[arg(...)], #[command(flatten)] from clap, etc.) belongs on the
        // variant body, not on the generated arms. Forwarding everything
        // would re-emit clap's own attrs into match arms that don't accept
        // them.
        let cfg_attrs: Vec<syn::Attribute> = variant
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("cfg"))
            .cloned()
            .collect();

        Ok(Self {
            ident,
            name_lit,
            group,
            batch,
            is_unit,
            cfg_attrs,
        })
    }

    fn pattern(&self, enum_ident: &Ident) -> TokenStream2 {
        let var = &self.ident;
        if self.is_unit {
            quote! { #enum_ident::#var }
        } else {
            quote! { #enum_ident::#var { .. } }
        }
    }

    fn variant_name_arm(&self, enum_ident: &Ident) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let pat = self.pattern(enum_ident);
        let name = &self.name_lit;
        quote! {
            #(#cfg)*
            #pat => #name
        }
    }

    fn batch_support_arm(&self, enum_ident: &Ident) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let pat = self.pattern(enum_ident);
        match self.batch {
            BatchSupportKind::Cli => quote! {
                #(#cfg)*
                #pat => crate::cli::BatchSupport::Cli
            },
            BatchSupportKind::Daemon => quote! {
                #(#cfg)*
                #pat => crate::cli::BatchSupport::Daemon
            },
            BatchSupportKind::Runtime => {
                let snake = to_snake_case(&self.ident.to_string());
                let helper = format_ident!("{}_batch_support", snake);
                let var_ident = &self.ident;
                if self.is_unit {
                    quote! {
                        #(#cfg)*
                        c @ #enum_ident::#var_ident => #helper(c)
                    }
                } else {
                    quote! {
                        #(#cfg)*
                        c @ #enum_ident::#var_ident { .. } => #helper(c)
                    }
                }
            }
        }
    }

    fn group_a_dispatch_arm(&self, enum_ident: &Ident) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let var = &self.ident;
        let snake = to_snake_case(&var.to_string());
        let dispatch_fn = format_ident!("cmd_{}_dispatch", snake);
        let pat = if self.is_unit {
            quote! { Some(c @ #enum_ident::#var) }
        } else {
            quote! { Some(c @ #enum_ident::#var { .. }) }
        };
        quote! {
            #(#cfg)*
            #pat => std::ops::ControlFlow::Break(crate::cli::commands::#dispatch_fn(cli, None, project_cqs_dir, c))
        }
    }

    fn group_b_dispatch_arm(&self, enum_ident: &Ident) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let var = &self.ident;
        let snake = to_snake_case(&var.to_string());
        let dispatch_fn = format_ident!("cmd_{}_dispatch", snake);
        let pat = if self.is_unit {
            quote! { Some(c @ #enum_ident::#var) }
        } else {
            quote! { Some(c @ #enum_ident::#var { .. }) }
        };
        quote! {
            #(#cfg)*
            #pat => crate::cli::commands::#dispatch_fn(cli, Some(ctx), project_cqs_dir, c)
        }
    }

    /// Emit a single line of the consolidated
    /// shim-existence guard. The guard is a `const _: ()` block that
    /// coerces every `cmd_<snake>_dispatch` through a typed local
    /// binding — if the function is missing or has the wrong shape, the
    /// error fires here once with the variant name in scope, not 58
    /// times across each generated match arm.
    fn shim_existence_check(&self) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let snake = to_snake_case(&self.ident.to_string());
        let dispatch_fn = format_ident!("cmd_{}_dispatch", snake);
        let var_doc_str = format!(
            "fn {dispatch_fn} must exist in `crate::cli::commands` with the \
             standardized dispatch signature — see `commands::dispatch_shims`"
        );
        let _ = var_doc_str; // expansion documentation; not emitted into source
        quote! {
            #(#cfg)*
            let _: DispatchFn = crate::cli::commands::#dispatch_fn;
        }
    }

    /// Emit a passthrough arm for variants of the OPPOSITE group:
    ///   * In `dispatch_group_a`, Group B variants → `Continue(())`.
    ///   * In `dispatch_group_b`, Group A variants → `unreachable!()`.
    fn passthrough_arm(&self, enum_ident: &Ident, in_group_a_match: bool) -> TokenStream2 {
        let cfg = &self.cfg_attrs;
        let pat = self.pattern(enum_ident);
        let name = &self.name_lit;
        if in_group_a_match {
            quote! {
                #(#cfg)*
                Some(#pat) => std::ops::ControlFlow::Continue(())
            }
        } else {
            quote! {
                #(#cfg)*
                Some(#pat) => unreachable!(
                    "Group A variant `{}` must be handled in dispatch_group_a",
                    #name
                )
            }
        }
    }
}

/// Convert `MyVariantName` → `my_variant_name`. Uses ASCII-only fast path
/// because every Rust identifier is ASCII.
fn to_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Convert `MyVariantName` → `my-variant-name`. Mirrors clap's default
/// subcommand naming so derived names match existing telemetry labels.
fn to_kebab_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{to_kebab_case, to_snake_case};

    #[test]
    fn snake_case_basic() {
        assert_eq!(to_snake_case("Init"), "init");
        assert_eq!(to_snake_case("TrainData"), "train_data");
        assert_eq!(to_snake_case("ImpactDiff"), "impact_diff");
        assert_eq!(to_snake_case("Ci"), "ci");
        assert_eq!(to_snake_case("AuditMode"), "audit_mode");
    }

    #[test]
    fn kebab_case_basic() {
        assert_eq!(to_kebab_case("Init"), "init");
        assert_eq!(to_kebab_case("TrainData"), "train-data");
        assert_eq!(to_kebab_case("ImpactDiff"), "impact-diff");
        assert_eq!(to_kebab_case("AuditMode"), "audit-mode");
        assert_eq!(to_kebab_case("ExportModel"), "export-model");
        assert_eq!(to_kebab_case("TrainPairs"), "train-pairs");
        assert_eq!(to_kebab_case("TestMap"), "test-map");
    }
}
