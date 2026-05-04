use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use serde_json::Value;
use syn::{
    parse_macro_input, FnArg, Ident, ItemTrait, Lit, Meta, Pat, ReturnType, TraitItem, Type,
};

struct ParamInfo {
    name: Ident,
    ty: Type,
    /// Extra attributes to emit on the struct field (e.g. `#[serde(rename = "coreType")]`).
    field_attrs: Vec<proc_macro2::TokenStream>,
}

struct TraitMethodInfo {
    fn_name: Ident,
    doc: String,
    params: Vec<ParamInfo>,
    result_type: Type,
    is_value_result: bool,
    example_params: proc_macro2::TokenStream,
    example_result: proc_macro2::TokenStream,
}

struct MethodInfo {
    rpc_name: String,
    fn_name: Ident,
    doc: String,
    params: Vec<ParamInfo>,
    result_type: Type,
    is_value_result: bool,
    example_params: proc_macro2::TokenStream,
    example_result: proc_macro2::TokenStream,
}

#[derive(Default)]
struct RpcSurfaceConfig {
    contract_path: Option<String>,
}

struct ContractMethodBinding {
    rpc_name: String,
    handler_name: String,
}

#[proc_macro_attribute]
pub fn rpc_surface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = match parse_rpc_surface_config(attr) {
        Ok(config) => config,
        Err(error) => return quote! { compile_error!(#error); }.into(),
    };
    let trait_def = parse_macro_input!(item as ItemTrait);
    let expanded = expand_rpc_surface(&trait_def, &config);
    expanded.into()
}

fn parse_rpc_surface_config(attr: TokenStream) -> Result<RpcSurfaceConfig, String> {
    let attr: proc_macro2::TokenStream = attr.into();
    if attr.is_empty() {
        return Ok(RpcSurfaceConfig::default());
    }

    let meta = syn::parse2::<Meta>(attr).map_err(|error| error.to_string())?;
    match meta {
        Meta::NameValue(name_value) if name_value.path.is_ident("contract") => {
            if let syn::Expr::Lit(expr_lit) = name_value.value {
                if let Lit::Str(value) = expr_lit.lit {
                    return Ok(RpcSurfaceConfig {
                        contract_path: Some(value.value()),
                    });
                }
            }
            Err("rpc_surface contract attribute must be a string literal".to_owned())
        }
        _ => Err("rpc_surface only supports `contract = \"...\"`".to_owned()),
    }
}

fn expand_rpc_surface(
    trait_def: &ItemTrait,
    config: &RpcSurfaceConfig,
) -> proc_macro2::TokenStream {
    let trait_name = &trait_def.ident;
    let vis = &trait_def.vis;
    let unsafety = &trait_def.unsafety;
    let trait_token = &trait_def.trait_token;
    let _brace_token = &trait_def.brace_token;

    let methods = match config.contract_path.as_deref() {
        Some(contract_path) => match bind_contract_methods(trait_def, contract_path) {
            Ok(methods) => methods,
            Err(error) => return quote! { compile_error!(#error); },
        },
        None => trait_def
            .items
            .iter()
            .filter_map(|item| {
                if let TraitItem::Fn(method) = item {
                    parse_attributed_method(method)
                } else {
                    None
                }
            })
            .collect(),
    };

    if methods.is_empty() {
        return quote! { compile_error!("#[rpc_surface] trait must define at least one RPC method"); };
    }

    // Rebuild trait methods with only doc attrs (strip #[method], #[example], #[field]).
    let clean_methods: Vec<proc_macro2::TokenStream> = trait_def
        .items
        .iter()
        .filter_map(|item| {
            if let TraitItem::Fn(method) = item {
                let mut sig = method.sig.clone();
                // Strip #[field] attrs from parameters
                for arg in &mut sig.inputs {
                    if let FnArg::Typed(pat_type) = arg {
                        pat_type.attrs.retain(|a| !a.path().is_ident("field"));
                    }
                }
                let doc_attrs: Vec<_> = method
                    .attrs
                    .iter()
                    .filter(|a| a.path().is_ident("doc"))
                    .collect();
                let semi = &method.semi_token;
                Some(quote! { #( #doc_attrs )* #sig #semi })
            } else {
                None
            }
        })
        .collect();

    let param_struct_defs: Vec<proc_macro2::TokenStream> =
        methods.iter().map(|m| generate_param_struct(m)).collect();

    let constants: Vec<proc_macro2::TokenStream> = methods
        .iter()
        .map(|m| {
            let const_name = rpc_name_to_const(&m.rpc_name);
            let rpc_name_str = &m.rpc_name;
            quote! { pub const #const_name: &str = #rpc_name_str; }
        })
        .collect();

    let all_names: Vec<proc_macro2::TokenStream> = methods
        .iter()
        .map(|m| {
            let const_name = rpc_name_to_const(&m.rpc_name);
            quote! { #const_name }
        })
        .collect();

    let dispatch_arms: Vec<proc_macro2::TokenStream> =
        methods.iter().map(|m| generate_dispatch_arm(m)).collect();

    let meta_entries: Vec<proc_macro2::TokenStream> =
        methods.iter().map(|m| generate_meta_entry(m)).collect();

    let rpc_defs = quote! {
        pub mod rpc_defs {
            use super::*;

            #( #constants )*

            pub const ALL: &[&str] = &[ #( #all_names ),* ];

            pub fn dispatch(
                method: &str,
                params: &serde_json::Value,
                handler: &dyn #trait_name,
            ) -> Option<Result<serde_json::Value, crate::protocol::RpcError>> {
                match method {
                    #( #dispatch_arms )*
                    _ => None,
                }
            }

            pub struct MethodMeta {
                pub name: &'static str,
                pub summary: &'static str,
                pub param_schema: serde_json::Value,
                pub result_schema: serde_json::Value,
                pub example_params: serde_json::Value,
                pub example_result: serde_json::Value,
            }

            pub fn get_method_meta() -> Vec<MethodMeta> {
                use schemars::schema_for;
                let to_val = |schema| {
                    let mut val: serde_json::Value = serde_json::to_value(&schema).unwrap();
                    if let Some(obj) = val.as_object_mut() {
                        obj.remove("$schema");
                    }
                    val
                };
                let value_schema = || serde_json::json!({"type": "object", "additionalProperties": true});

                vec![
                    #( #meta_entries )*
                ]
            }
        }
    };

    quote! {
        #( #param_struct_defs )*

        #vis #unsafety #trait_token #trait_name {
            #( #clean_methods )*
        }

        #rpc_defs
    }
}

fn bind_contract_methods(
    trait_def: &ItemTrait,
    contract_path: &str,
) -> Result<Vec<MethodInfo>, String> {
    let bindings = load_contract_methods(contract_path)?;
    let mut trait_methods = BTreeMap::new();

    for item in &trait_def.items {
        if let TraitItem::Fn(method) = item {
            let trait_method = parse_trait_method(method);
            let key = trait_method.fn_name.to_string();
            if trait_methods.insert(key.clone(), trait_method).is_some() {
                return Err(format!(
                    "duplicate trait method name `{key}` in #[rpc_surface] trait"
                ));
            }
        }
    }

    let mut resolved = Vec::new();
    for binding in bindings {
        let Some(method) = trait_methods.remove(&binding.handler_name) else {
            return Err(format!(
                "contract handler `{}` does not match any trait method in {}",
                binding.handler_name, contract_path
            ));
        };
        resolved.push(MethodInfo {
            rpc_name: binding.rpc_name,
            fn_name: method.fn_name,
            doc: method.doc,
            params: method.params,
            result_type: method.result_type,
            is_value_result: method.is_value_result,
            example_params: method.example_params,
            example_result: method.example_result,
        });
    }

    if !trait_methods.is_empty() {
        let extras = trait_methods.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "trait methods missing from contract descriptor {}: {}",
            contract_path, extras
        ));
    }

    Ok(resolved)
}

fn load_contract_methods(contract_path: &str) -> Result<Vec<ContractMethodBinding>, String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("missing CARGO_MANIFEST_DIR for #[rpc_surface]: {error}"))?;
    let path = PathBuf::from(manifest_dir).join(contract_path);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read contract {}: {error}", path.display()))?;
    let contract: Value = serde_json::from_str(&text)
        .map_err(|error| format!("failed to parse contract {}: {error}", path.display()))?;
    let methods = contract["methods"]
        .as_array()
        .ok_or_else(|| format!("contract {} is missing a `methods` array", path.display()))?;

    let mut seen_handlers = BTreeMap::<String, String>::new();
    let mut bindings = Vec::new();
    for method in methods {
        let rpc_name = method["name"]
            .as_str()
            .ok_or_else(|| format!("contract {} method is missing `name`", path.display()))?
            .to_owned();
        let handler_name = method["handler"]
            .as_str()
            .ok_or_else(|| {
                format!(
                    "contract {} method {} is missing `handler`",
                    path.display(),
                    rpc_name
                )
            })?
            .to_owned();

        if let Some(existing) = seen_handlers.insert(handler_name.clone(), rpc_name.clone()) {
            return Err(format!(
                "contract {} maps handler `{}` to both `{}` and `{}`",
                path.display(),
                handler_name,
                existing,
                rpc_name
            ));
        }

        bindings.push(ContractMethodBinding {
            rpc_name,
            handler_name,
        });
    }

    Ok(bindings)
}

fn parse_attributed_method(method: &syn::TraitItemFn) -> Option<MethodInfo> {
    let rpc_name = method.attrs.iter().find_map(|attr| {
        if attr.path().is_ident("method") {
            if let Meta::List(list) = &attr.meta {
                let lit: Lit = syn::parse2(list.tokens.clone()).ok()?;
                if let Lit::Str(s) = lit {
                    return Some(s.value());
                }
            }
        }
        None
    })?;
    let method = parse_trait_method(method);
    Some(MethodInfo {
        rpc_name,
        fn_name: method.fn_name,
        doc: method.doc,
        params: method.params,
        result_type: method.result_type,
        is_value_result: method.is_value_result,
        example_params: method.example_params,
        example_result: method.example_result,
    })
}

fn parse_trait_method(method: &syn::TraitItemFn) -> TraitMethodInfo {
    let fn_name = method.sig.ident.clone();

    let doc = method
        .attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                if let Meta::NameValue(nv) = &attr.meta {
                    if let syn::Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(s) = &expr_lit.lit {
                            return Some(s.value().trim().to_string());
                        }
                    }
                }
            }
            None
        })
        .collect::<Vec<_>>()
        .join(" ");

    // Extract #[example("params_json", "result_json")]
    let (example_params, example_result) = method
        .attrs
        .iter()
        .find_map(|attr| {
            if attr.path().is_ident("example") {
                if let Meta::List(list) = &attr.meta {
                    let tts: Vec<proc_macro2::TokenTree> =
                        list.tokens.clone().into_iter().collect();
                    // Collect all string literals (skip commas)
                    let strs: Vec<String> = tts
                        .iter()
                        .filter_map(|tt| {
                            if let proc_macro2::TokenTree::Literal(lit) = tt {
                                let s = lit.to_string();
                                if s.starts_with('"') && s.ends_with('"') {
                                    let inner = &s[1..s.len() - 1];
                                    return Some(inner.replace("\\\"", "\""));
                                }
                            }
                            None
                        })
                        .collect();
                    let params = strs
                        .first()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                        .map(|val| {
                            let lit = proc_macro2::Literal::string(&val.to_string());
                            quote! { {
                                let __s: &str = #lit;
                                serde_json::from_str::<serde_json::Value>(__s).unwrap_or(serde_json::Value::Null)
                            } }
                        });
                    let result = strs
                        .get(1)
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                        .map(|val| {
                            let lit = proc_macro2::Literal::string(&val.to_string());
                            quote! { {
                                let __s: &str = #lit;
                                serde_json::from_str::<serde_json::Value>(__s).unwrap_or(serde_json::Value::Null)
                            } }
                        });
                    Some((
                        params.unwrap_or_else(|| quote! { serde_json::json!({}) }),
                        result.unwrap_or_else(|| quote! { serde_json::json!({}) }),
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            (
                quote! { serde_json::json!({}) },
                quote! { serde_json::json!({}) },
            )
        });

    // Extract parameters (skip `&self`), including #[field(...)] attributes.
    let params: Vec<ParamInfo> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    // Collect #[field(...)] attributes and convert to #[serde(...)]
                    let field_attrs: Vec<proc_macro2::TokenStream> = pat_type
                        .attrs
                        .iter()
                        .filter_map(|attr| {
                            if attr.path().is_ident("field") {
                                if let Meta::List(list) = &attr.meta {
                                    let inner = &list.tokens;
                                    Some(quote! { #[serde(#inner)] })
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .collect();

                    return Some(ParamInfo {
                        name: pat_ident.ident.clone(),
                        ty: (*pat_type.ty).clone(),
                        field_attrs,
                    });
                }
            }
            None
        })
        .collect();

    // Extract result type from `-> Result<T, _>`.
    let (result_type, is_value_result) = match &method.sig.output {
        ReturnType::Type(_, ty) => {
            if let Type::Path(type_path) = ty.as_ref() {
                let extracted = type_path
                    .path
                    .segments
                    .iter()
                    .find(|seg| seg.ident == "Result")
                    .and_then(|seg| {
                        if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                            args.args.first().and_then(|arg| {
                                if let syn::GenericArgument::Type(ty) = arg {
                                    Some(ty.clone())
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    });
                match extracted {
                    Some(inner) => {
                        let is_val = is_value_type(&inner);
                        (inner, is_val)
                    }
                    None => (syn::parse2(quote! { serde_json::Value }).unwrap(), true),
                }
            } else {
                (syn::parse2(quote! { serde_json::Value }).unwrap(), true)
            }
        }
        _ => (syn::parse2(quote! { serde_json::Value }).unwrap(), true),
    };

    TraitMethodInfo {
        fn_name,
        doc,
        params,
        result_type,
        is_value_result,
        example_params,
        example_result,
    }
}

fn generate_param_struct(method: &MethodInfo) -> proc_macro2::TokenStream {
    let struct_name = format_ident!("{}Params", pascal_case(&method.fn_name.to_string()));
    let fields: Vec<proc_macro2::TokenStream> = method
        .params
        .iter()
        .map(|p| {
            let name = &p.name;
            let ty = &p.ty;
            let attrs = &p.field_attrs;
            quote! {
                #( #attrs )*
                pub #name: #ty
            }
        })
        .collect();

    if fields.is_empty() {
        quote! {
            #[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema)]
            pub struct #struct_name {}
        }
    } else {
        quote! {
            #[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema)]
            pub struct #struct_name {
                #( #fields, )*
            }
        }
    }
}

fn generate_dispatch_arm(method: &MethodInfo) -> proc_macro2::TokenStream {
    let rpc_name = &method.rpc_name;
    let fn_name = &method.fn_name;
    let struct_name = format_ident!("{}Params", pascal_case(&method.fn_name.to_string()));

    // When the trait method returns Result<Value, _>, skip serde_json::to_value
    // to avoid a redundant clone — the value is already serialized.
    let convert: proc_macro2::TokenStream = if method.is_value_result {
        quote! { |r| r }
    } else {
        quote! { |r| serde_json::to_value(&r).unwrap_or(serde_json::Value::Null) }
    };

    if method.params.is_empty() {
        quote! {
            #rpc_name => {
                Some(handler.#fn_name().map(#convert))
            }
        }
    } else {
        let param_names: Vec<&Ident> = method.params.iter().map(|p| &p.name).collect();
        quote! {
            #rpc_name => {
                let effective = if params.is_null() {
                    &serde_json::Value::Object(Default::default())
                } else {
                    params
                };
                let parsed: Result<#struct_name, _> = serde_json::from_value(effective.clone());
                Some(match parsed {
                    Ok(p) => handler.#fn_name(#( p.#param_names, )*) .map(#convert),
                    Err(e) => Err(crate::protocol::RpcError {
                        code: crate::protocol::INVALID_PARAMS,
                        message: format!("invalid params: {e}"),
                        data: None,
                    }),
                })
            }
        }
    }
}

fn generate_meta_entry(method: &MethodInfo) -> proc_macro2::TokenStream {
    let rpc_name = &method.rpc_name;
    let doc = &method.doc;
    let struct_name = format_ident!("{}Params", pascal_case(&method.fn_name.to_string()));
    let example_params = &method.example_params;
    let example_result = &method.example_result;

    if method.is_value_result {
        quote! {
            MethodMeta {
                name: #rpc_name,
                summary: #doc,
                param_schema: to_val(schema_for!(#struct_name)),
                result_schema: value_schema(),
                example_params: #example_params,
                example_result: #example_result,
            },
        }
    } else {
        let result_type = &method.result_type;
        quote! {
            MethodMeta {
                name: #rpc_name,
                summary: #doc,
                param_schema: to_val(schema_for!(#struct_name)),
                result_schema: to_val(schema_for!(#result_type)),
                example_params: #example_params,
                example_result: #example_result,
            },
        }
    }
}

fn is_value_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        type_path
            .path
            .segments
            .last()
            .map(|s| s.ident == "Value")
            .unwrap_or(false)
    } else {
        false
    }
}

fn rpc_name_to_const(name: &str) -> Ident {
    let upper: String = name
        .chars()
        .map(|c| {
            if c == '.' || c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    format_ident!("{}", upper)
}

fn pascal_case(name: &str) -> String {
    name.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + &chars.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect()
}
