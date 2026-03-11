//! LLM推理组件 — proc macro实现
//!
//! ToMarkdown: struct → markdown prompt文本（支持嵌套struct、Vec、基本类型）
//! FromMarkdown: enum/struct → markdown输出解析

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Lit, Meta};

// ============================================================
// Type classification (shared by ToMarkdown and FromMarkdown)
// ============================================================

enum FieldKind {
    String,
    Bool,
    Numeric,
    Vec(syn::Type),
    Option(Box<FieldKind>),
    Other,
}

fn classify_field_type(ty: &syn::Type) -> FieldKind {
    if let syn::Type::Path(type_path) = ty {
        if let Some(seg) = type_path.path.segments.last() {
            let name = seg.ident.to_string();
            match name.as_str() {
                "String" => return FieldKind::String,
                "bool" => return FieldKind::Bool,
                "u8" | "u16" | "u32" | "u64" | "usize" |
                "i8" | "i16" | "i32" | "i64" | "isize" |
                "f32" | "f64" => return FieldKind::Numeric,
                "Vec" => {
                    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            return FieldKind::Vec(inner.clone());
                        }
                    }
                }
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                            let inner_kind = classify_field_type(inner);
                            return FieldKind::Option(Box::new(inner_kind));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    FieldKind::Other
}


// ============================================================
// ToMarkdown derive
// ============================================================

/// Collected info about a struct field for ToMarkdown generation.
struct ToMarkdownField {
    ident: syn::Ident,
    kind: FieldKind,
    section_title: String,
    skip: bool,
}

/// Generate `impl ToMarkdown for Struct` from derive attributes.
pub fn derive_to_markdown(input: DeriveInput) -> TokenStream {
    let struct_name = &input.ident;

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(struct_name, "ToMarkdown only supports structs with named fields")
                    .to_compile_error();
            }
        },
        _ => {
            return syn::Error::new_spanned(struct_name, "ToMarkdown only supports structs")
                .to_compile_error();
        }
    };

    // Extract struct-level doc comment as header
    let struct_docs = extract_doc_comments(&input.attrs);
    let struct_header = struct_docs.join(" ");

    let mut tm_fields: Vec<ToMarkdownField> = Vec::new();
    for field in fields.iter() {
        let ident = field.ident.clone().unwrap();
        let skip = has_markdown_skip(&field.attrs);
        let docs = extract_doc_comments(&field.attrs);
        let section_title = if docs.is_empty() {
            ident.to_string()
        } else {
            docs.join(" ")
        };
        let kind = classify_field_type(&field.ty);
        tm_fields.push(ToMarkdownField { ident, kind, section_title, skip });
    }

    // Name collision detection: case-insensitive check struct name vs field names
    {
        let struct_lower = struct_name.to_string().to_lowercase();
        for f in &tm_fields {
            if f.ident.to_string().to_lowercase() == struct_lower {
                return syn::Error::new_spanned(
                    &f.ident,
                    format!("field name '{}' collides with struct name '{}' (case-insensitive). This would cause ambiguous separators in markdown format.", f.ident, struct_name)
                ).to_compile_error();
            }
        }
    }

    let depth_renders: Vec<TokenStream> = tm_fields.iter().map(|f| {
        if f.skip { return quote! {}; }
        gen_depth_render(f)
    }).collect();

    let item_renders: Vec<TokenStream> = tm_fields.iter().map(|f| {
        if f.skip { return quote! {}; }
        gen_item_render(f)
    }).collect();

    let header_code = if struct_header.is_empty() {
        quote! {}
    } else {
        quote! {
            __out.push_str(#struct_header);
            __out.push('\n');
        }
    };

    quote! {
        impl ::mad_hatter::llm::ToMarkdown for #struct_name {
            fn to_markdown_depth(&self, __depth: usize) -> ::std::string::String {
                let mut __out = ::std::string::String::new();
                #header_code
                #(#depth_renders)*
                __out
            }
            fn to_markdown_item(&self) -> ::std::string::String {
                let mut __out = ::std::string::String::new();
                #(#item_renders)*
                __out
            }
        }
        impl ::mad_hatter::llm::StructInput for #struct_name {}
    }
}

/// Generate rendering code for a field in to_markdown_depth mode (heading-based).
fn gen_depth_render(f: &ToMarkdownField) -> TokenStream {
    let ident = &f.ident;
    let title = &f.section_title;

    match &f.kind {
        FieldKind::String => {
            quote! {
                {
                    let __val = &self.#ident;
                    if !__val.is_empty() {
                        let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                        __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                        __out.push_str(__val);
                        __out.push('\n');
                    }
                }
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                {
                    let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                    __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                    __out.push_str(&self.#ident.to_string());
                    __out.push('\n');
                }
            }
        }
        FieldKind::Vec(inner_ty) => {
            let inner_kind = classify_field_type(inner_ty);
            let element_render = gen_vec_element_render_depth(&inner_kind);
            quote! {
                {
                    let __items = &self.#ident;
                    if !__items.is_empty() {
                        let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                        __out.push_str(&::std::format!("\n{} {} {}\n\n", __hashes, #title, __hashes));
                        for (__i, __item) in __items.iter().enumerate() {
                            if __i > 0 { __out.push('\n'); }
                            #element_render
                        }
                    }
                }
            }
        }
        FieldKind::Option(inner_kind) => {
            gen_option_depth_render(ident, title, inner_kind)
        }
        FieldKind::Other => {
            quote! {
                {
                    let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                    __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                    __out.push_str(&self.#ident.to_markdown_depth(__depth + 1));
                }
            }
        }
    }
}

/// Generate code to render a single Vec element in depth mode.
fn gen_vec_element_render_depth(inner_kind: &FieldKind) -> TokenStream {
    match inner_kind {
        FieldKind::String => {
            quote! { __out.push_str(__item); __out.push('\n'); }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! { __out.push_str(&__item.to_string()); __out.push('\n'); }
        }
        _ => {
            // Nested struct or other: use compact item format
            quote! { __out.push_str(&__item.to_markdown_item()); }
        }
    }
}

/// Generate depth-mode rendering for Option<T> fields.
fn gen_option_depth_render(ident: &syn::Ident, title: &str, inner_kind: &FieldKind) -> TokenStream {
    let value_render = match inner_kind {
        FieldKind::String => {
            quote! {
                if !__val.is_empty() {
                    let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                    __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                    __out.push_str(__val);
                    __out.push('\n');
                }
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                __out.push_str(&__val.to_string());
                __out.push('\n');
            }
        }
        _ => {
            quote! {
                let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                __out.push_str(&__val.to_markdown_depth(__depth + 1));
            }
        }
    };

    quote! {
        if let ::std::option::Option::Some(__val) = &self.#ident {
            #value_render
        }
    }
}

/// Generate rendering code for a field in to_markdown_item mode (compact "field: value" format).
fn gen_item_render(f: &ToMarkdownField) -> TokenStream {
    let ident = &f.ident;
    let field_name = ident.to_string();

    match &f.kind {
        FieldKind::String => {
            quote! {
                {
                    let __val = &self.#ident;
                    if !__val.is_empty() {
                        __out.push_str(#field_name);
                        __out.push_str(": ");
                        __out.push_str(__val);
                        __out.push('\n');
                    }
                }
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                {
                    __out.push_str(#field_name);
                    __out.push_str(": ");
                    __out.push_str(&self.#ident.to_string());
                    __out.push('\n');
                }
            }
        }
        FieldKind::Vec(inner_ty) => {
            let inner_kind = classify_field_type(inner_ty);
            let element_render = gen_vec_element_render_item(&inner_kind);
            quote! {
                {
                    let __items = &self.#ident;
                    if !__items.is_empty() {
                        __out.push_str(#field_name);
                        __out.push_str(":\n");
                        for __item in __items.iter() {
                            #element_render
                        }
                    }
                }
            }
        }
        FieldKind::Option(inner_kind) => {
            gen_option_item_render(ident, &field_name, inner_kind)
        }
        FieldKind::Other => {
            quote! {
                {
                    __out.push_str(#field_name);
                    __out.push_str(": ");
                    __out.push_str(&self.#ident.to_markdown_item());
                    __out.push('\n');
                }
            }
        }
    }
}

/// Generate code to render a single Vec element in item mode.
fn gen_vec_element_render_item(inner_kind: &FieldKind) -> TokenStream {
    match inner_kind {
        FieldKind::String => {
            quote! { __out.push_str(__item); __out.push('\n'); }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! { __out.push_str(&__item.to_string()); __out.push('\n'); }
        }
        _ => {
            quote! { __out.push_str(&__item.to_markdown_item()); __out.push('\n'); }
        }
    }
}

/// Generate item-mode rendering for Option<T> fields.
fn gen_option_item_render(ident: &syn::Ident, field_name: &str, inner_kind: &FieldKind) -> TokenStream {
    let value_render = match inner_kind {
        FieldKind::String => {
            quote! {
                if !__val.is_empty() {
                    __out.push_str(#field_name);
                    __out.push_str(": ");
                    __out.push_str(__val);
                    __out.push('\n');
                }
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                __out.push_str(#field_name);
                __out.push_str(": ");
                __out.push_str(&__val.to_string());
                __out.push('\n');
            }
        }
        _ => {
            quote! {
                __out.push_str(#field_name);
                __out.push_str(": ");
                __out.push_str(&__val.to_markdown_item());
                __out.push('\n');
            }
        }
    };

    quote! {
        if let ::std::option::Option::Some(__val) = &self.#ident {
            #value_render
        }
    }
}

// ============================================================
// FromMarkdown derive
// ============================================================

/// Information about a single enum variant, extracted at macro time.
struct VariantInfo {
    ident: syn::Ident,
    snake_name: String,
    doc: String,
    fields: Vec<FromFieldInfo>,
}

/// Information about a single field within a variant.
struct FromFieldInfo {
    is_required: bool,
    name: String,
    doc: String,
    is_option: bool,
    is_vec: bool,
    /// The inner type name for Option fields (e.g. "u64", "String")
    inner_type: String,
    /// The inner type name for Vec fields (e.g. "ReplaceBlock")
    vec_inner_type: String,
    /// The syn::Type for Vec inner type (for generating code)
    vec_inner_syn_type: Option<syn::Type>,
}

/// Generate `impl FromMarkdown for Enum` or `impl FromMarkdown for Struct`.
pub fn derive_from_markdown(input: DeriveInput) -> TokenStream {
    match &input.data {
        Data::Enum(_) => derive_from_markdown_enum(input),
        Data::Struct(_) => derive_from_markdown_struct(input),
        _ => {
            syn::Error::new_spanned(&input.ident, "FromMarkdown only supports enums and structs")
                .to_compile_error()
        }
    }
}

// ============================================================
// FromMarkdown for enum
// ============================================================

fn derive_from_markdown_enum(input: DeriveInput) -> TokenStream {
    let enum_name = &input.ident;
    let enum_name_str = enum_name.to_string();

    let variants_data = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => unreachable!(),
    };

    let mut variants: Vec<VariantInfo> = Vec::new();
    for v in variants_data {
        let docs = extract_doc_comments(&v.attrs);
        let doc = docs.join(" ");
        let snake_name = to_snake_case(&v.ident.to_string());

        let fields = match &v.fields {
            Fields::Named(named) => {
                named.named.iter().map(|f| {
                    let fname = f.ident.as_ref().unwrap().to_string();
                    let fdocs = extract_doc_comments(&f.attrs);
                    let fdoc = if fdocs.is_empty() { fname.clone() } else { fdocs.join(" ") };
                    let (is_option, inner_type) = check_option_type(&f.ty);
                    let (is_vec, vec_inner_type, vec_inner_syn_type) = check_vec_type(&f.ty);
                    let is_required = has_markdown_required(&f.attrs);
                    FromFieldInfo { name: fname, doc: fdoc, is_option, is_vec, inner_type, vec_inner_type, vec_inner_syn_type, is_required }
                }).collect()
            }
            Fields::Unit => Vec::new(),
            _ => {
                return syn::Error::new_spanned(&v.ident, "FromMarkdown only supports unit or named-field variants")
                    .to_compile_error();
            }
        };

        variants.push(VariantInfo {
            ident: v.ident.clone(),
            snake_name,
            doc,
            fields,
        });
    }

    // Name collision detection (case-insensitive)
    {
        let type_lower = enum_name_str.to_lowercase();
        for vi in &variants {
            for f in &vi.fields {
                if f.name.to_lowercase() == type_lower {
                    return syn::Error::new_spanned(
                        enum_name,
                        format!("FromMarkdown: field name '{}' collides with type name '{}' (case-insensitive). This would cause ambiguous separators.", f.name, enum_name_str)
                    ).to_compile_error();
                }
            }
        }
    }

    let schema_body = gen_schema_markdown(&enum_name_str, &variants);
    let parse_body = gen_from_markdown(enum_name, &enum_name_str, &variants);
    quote! {
        impl ::mad_hatter::llm::FromMarkdown for #enum_name {
            fn schema_markdown(token: &str) -> ::std::string::String {
                #schema_body
            }
            fn from_markdown(text: &str, token: &str) -> ::std::result::Result<::std::vec::Vec<Self>, ::std::string::String> {
                #parse_body
            }
            fn type_name() -> &'static str {
                #enum_name_str
            }
        }
        impl ::mad_hatter::llm::StructOutput for #enum_name {}
    }
}

/// Generate the body of `schema_markdown` for enum.
fn gen_schema_markdown(enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    let mut variant_schemas = Vec::new();

    for vi in variants {
        let snake = &vi.snake_name;
        let doc = &vi.doc;
        let required_fields: Vec<&FromFieldInfo> = vi.fields.iter().filter(|f| !f.is_option).collect();
        let option_fields: Vec<&FromFieldInfo> = vi.fields.iter().filter(|f| f.is_option).collect();
        let total = vi.fields.len();

        if total == 0 {
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n{ename}-{token}\n{snake}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake
                ));
            });
        } else if total == 1 && option_fields.len() == 1 {
            let fdoc = &option_fields[0].doc;
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n{ename}-{token}\n{snake}\n{fdoc}（可选）\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                ));
            });
        } else if total == 1 && required_fields.len() == 1 {
            let f = &required_fields[0];
            let fdoc = &f.doc;
            if f.is_vec {
                let vec_type = &f.vec_inner_type;
                variant_schemas.push(quote! {
                    __out.push_str(&::std::format!(
                        "action {doc}\n{ename}-{token}\n{snake}\n{fdoc}（包含嵌套{vtype}元素）\n",
                        doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc, vtype = #vec_type
                    ));
                });
            } else {
                variant_schemas.push(quote! {
                    __out.push_str(&::std::format!(
                        "action {doc}\n{ename}-{token}\n{snake}\n{fdoc}（多行）\n",
                        doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                    ));
                });
            }
        } else {
            let mut field_lines = Vec::new();
            for f in &vi.fields {
                let fname = &f.name;
                let fdoc = &f.doc;
                if f.is_vec {
                    let vec_type = &f.vec_inner_type;
                    field_lines.push(quote! {
                        __out.push_str(&::std::format!(
                            "{fname}-{token}\n{fdoc}（多个{vtype}元素，用{vtype}-{token}分隔）\n",
                            fname = #fname, token = token, fdoc = #fdoc, vtype = #vec_type
                        ));
                    });
                } else if f.is_option {
                    field_lines.push(quote! {
                        __out.push_str(&::std::format!(
                            "{fname}-{token}\n{fdoc}（可选）\n",
                            fname = #fname, token = token, fdoc = #fdoc
                        ));
                    });
                } else {
                    field_lines.push(quote! {
                        __out.push_str(&::std::format!(
                            "{fname}-{token}\n{fdoc}\n",
                            fname = #fname, token = token, fdoc = #fdoc
                        ));
                    });
                }
            }
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n{ename}-{token}\n{snake}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake
                ));
                #(#field_lines)*
            });
        }

        variant_schemas.push(quote! {
            __out.push('\n');
        });
    }

    quote! {
        let mut __out = ::std::string::String::new();
        #(#variant_schemas)*
        __out.push_str(&::std::format!("最后输出: {ename}-end-{token}\n", ename = #enum_name_str, token = token));
        __out
    }
}

/// Generate the body of `from_markdown` for enum.
fn gen_from_markdown(enum_name: &syn::Ident, enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    let mut match_arms = Vec::new();
    let variant_list_str = variants.iter().map(|v| v.snake_name.as_str()).collect::<Vec<&str>>().join(", ");

    for vi in variants {
        let snake = &vi.snake_name;
        let variant_ident = &vi.ident;
        let field_count = vi.fields.len();

        if field_count == 0 {
            match_arms.push(quote! {
                #snake => {
                    __results.push(#enum_name::#variant_ident);
                }
            });
        } else if field_count == 1 {
            let f = &vi.fields[0];
            let field_ident = syn::Ident::new(&f.name, proc_macro2::Span::call_site());

            if f.is_option {
                let inner = &f.inner_type;
                let fname_str = &f.name;
                if is_numeric_inner_type(inner) {
                    match_arms.push(quote! {
                        #snake => {
                            let __val_str = __body.trim();
                            let __opt = if __val_str.is_empty() {
                                ::std::option::Option::None
                            } else {
                                match __val_str.parse() {
                                    Ok(v) => ::std::option::Option::Some(v),
                                    Err(e) => return ::std::result::Result::Err(
                                        ::std::format!("[{}] Failed to parse optional field '{}' of variant '{}': {}", #enum_name_str, #fname_str, #snake, e)
                                    ),
                                }
                            };
                            __results.push(#enum_name::#variant_ident { #field_ident: __opt });
                        }
                    });
                } else {
                    match_arms.push(quote! {
                        #snake => {
                            let __val_str = __body.trim();
                            let __opt = if __val_str.is_empty() {
                                ::std::option::Option::None
                            } else {
                                ::std::option::Option::Some(__val_str.to_string())
                            };
                            __results.push(#enum_name::#variant_ident { #field_ident: __opt });
                        }
                    });
                }
            } else if f.is_vec {
                let vec_inner_ty = f.vec_inner_syn_type.as_ref().unwrap();
                match_arms.push(quote! {
                    #snake => {
                        let __vec_items = <#vec_inner_ty as ::mad_hatter::llm::FromMarkdown>::from_markdown(__body, token)?;
                        __results.push(#enum_name::#variant_ident { #field_ident: __vec_items });
                    }
                });
            } else {
                let fname_str = &f.name;
                if f.is_required {
                    match_arms.push(quote! {
                        #snake => {
                            let __val = __body.trim_end().to_string();
                            if __val.trim().is_empty() {
                                return ::std::result::Result::Err(
                                    ::std::format!("[{}] Required field '{}' of variant '{}' is empty", #enum_name_str, #fname_str, #snake)
                                );
                            }
                            __results.push(#enum_name::#variant_ident { #field_ident: __val });
                        }
                    });
                } else {
                    match_arms.push(quote! {
                        #snake => {
                            __results.push(#enum_name::#variant_ident { #field_ident: __body.trim_end().to_string() });
                        }
                    });
                }
            }
        } else {
            let multi_parse = gen_multi_field_parse(enum_name, enum_name_str, vi);
            match_arms.push(quote! {
                #snake => {
                    #multi_parse
                }
            });
        }
    }

    quote! {
        let __stripped = ::mad_hatter::llm::strip_code_block(text);
        let text = __stripped.as_str();

        let mut __results: ::std::vec::Vec<#enum_name> = ::std::vec::Vec::new();
        let __element_sep = ::std::format!("{}-{}", #enum_name_str, token);
        let __end_marker = ::std::format!("{}-end-{}", #enum_name_str, token);

        let __text_trimmed = text.trim();

        // Check end marker
        if !__text_trimmed.ends_with(&__end_marker) {
            let __tail: ::std::string::String = if __text_trimmed.len() > 200 {
                ::std::format!("...{}", &__text_trimmed[__text_trimmed.len() - 200..])
            } else {
                __text_trimmed.to_string()
            };
            if __text_trimmed.contains(&__element_sep) {
                return ::std::result::Result::Err(
                    ::std::format!("[{}] Missing end marker '{}'. Content tail (up to 200 chars): {}", #enum_name_str, __end_marker, __tail)
                );
            } else {
                return ::std::result::Result::Err(
                    ::std::format!("[{}] No valid element found. Expected '{}' to start output. Content (up to 200 chars): {}", #enum_name_str, __element_sep, __tail)
                );
            }
        }

        // Remove end marker
        let __text_body = __text_trimmed[..__text_trimmed.len() - __end_marker.len()].trim();

        // Split by element separator
        let __sep_with_newline = ::std::format!("{}\n", __element_sep);
        let __chunks: ::std::vec::Vec<&str> = __text_body.split(&__sep_with_newline).collect();

        let mut __parsed_count: usize = 0;

        for __chunk in __chunks {
            let __chunk = __chunk.trim();
            if __chunk.is_empty() { continue; }

            // First line is variant name
            let (__variant_line, __body) = match __chunk.find('\n') {
                Some(pos) => (__chunk[..pos].trim(), &__chunk[pos + 1..]),
                None => (__chunk.trim(), ""),
            };

            match __variant_line {
                #(#match_arms)*
                _ => {
                    return ::std::result::Result::Err(
                        ::std::format!("[{}] Unknown variant '{}'. Parsed {} element(s) successfully. Expected one of: {}", #enum_name_str, __variant_line, __parsed_count, #variant_list_str)
                    );
                }
            }

            __parsed_count += 1;
        }

        ::std::result::Result::Ok(__results)
    }
}

/// Generate parsing code for a variant with N>=2 fields.
fn gen_multi_field_parse(enum_name: &syn::Ident, enum_name_str: &str, vi: &VariantInfo) -> TokenStream {
    let variant_ident = &vi.ident;
    let snake = &vi.snake_name;

    let mut field_names: Vec<String> = Vec::new();
    let mut field_idents: Vec<syn::Ident> = Vec::new();

    for f in &vi.fields {
        field_names.push(f.name.clone());
        field_idents.push(syn::Ident::new(&f.name, proc_macro2::Span::call_site()));
    }

    let field_count = field_names.len();
    let sep_name_literals: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();

    let mut field_extractions = Vec::new();
    let mut field_var_names: Vec<syn::Ident> = Vec::new();

    for (i, f) in vi.fields.iter().enumerate() {
        let var_name = syn::Ident::new(&format!("__field_{}", f.name), proc_macro2::Span::call_site());
        let fname = &f.name;

        if f.is_vec {
            // Vec<T> field: call T::from_markdown on the extracted text
            let vec_inner_ty = f.vec_inner_syn_type.as_ref().unwrap();
            field_extractions.push(quote! {
                let #var_name = {
                    let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                    <#vec_inner_ty as ::mad_hatter::llm::FromMarkdown>::from_markdown(__raw, token)?
                };
            });
        } else if f.is_option {
            let inner = &f.inner_type;
            if is_numeric_inner_type(inner) {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __raw.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("[{}] Failed to parse field '{}' of variant '{}': {}", #enum_name_str, #fname, #snake, e)
                                ),
                            }
                        }
                    };
                });
            } else {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim_end()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(__raw.to_string())
                        }
                    };
                });
            }
        } else {
            let fname_str = &f.name;
            if f.is_required {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                        let __val = if #i == #field_count - 1 {
                            __raw.trim_end().to_string()
                        } else {
                            __raw.trim_end_matches('\n').to_string()
                        };
                        if __val.trim().is_empty() {
                            return ::std::result::Result::Err(
                                ::std::format!("[{}] Required field '{}' of variant '{}' is empty", #enum_name_str, #fname_str, #snake)
                            );
                        }
                        __val
                    };
                });
            } else {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                        if #i == #field_count - 1 {
                            __raw.trim_end().to_string()
                        } else {
                            __raw.trim_end_matches('\n').to_string()
                        }
                    };
                });
            }
        }

        field_var_names.push(var_name);
    }

    let constructor_fields: Vec<TokenStream> = field_idents.iter().zip(field_var_names.iter()).map(|(fi, vn)| {
        quote! { #fi: #vn }
    }).collect();

    quote! {
        let __seps: ::std::vec::Vec<::std::string::String> = [#(#sep_name_literals),*]
            .iter()
            .map(|n| ::std::format!("{}-{}", n, token))
            .collect();

        let mut __field_values: ::std::vec::Vec<&str> = ::std::vec::Vec::new();
        let mut __remaining = __body;

        for (i, sep) in __seps.iter().enumerate() {
            let __sep_with_newline = ::std::format!("{}\n", sep);
            match __remaining.find(&__sep_with_newline) {
                Some(pos) => {
                    __remaining = &__remaining[pos + __sep_with_newline.len()..];
                }
                None => {
                    if __remaining.trim_end() == sep.as_str() {
                        __remaining = "";
                    } else {
                        __field_values.push("");
                        continue;
                    }
                }
            }

            let mut __end = __remaining.len();
            for next_sep in __seps.iter().skip(i + 1) {
                let __next_sep_with_newline = ::std::format!("{}\n", next_sep);
                if let Some(pos) = __remaining.find(&__next_sep_with_newline) {
                    __end = pos;
                    break;
                }
                let __trimmed = __remaining.trim_end();
                if __trimmed.ends_with(next_sep.as_str()) {
                    __end = __trimmed.len() - next_sep.len();
                    break;
                }
            }

            __field_values.push(&__remaining[..__end]);
            __remaining = &__remaining[__end..];
        }

        #(#field_extractions)*

        __results.push(#enum_name::#variant_ident {
            #(#constructor_fields),*
        });
    }
}

// ============================================================
// FromMarkdown for struct
// ============================================================

fn derive_from_markdown_struct(input: DeriveInput) -> TokenStream {
    let struct_name = &input.ident;
    let struct_name_str = struct_name.to_string();

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(struct_name, "FromMarkdown for struct only supports named fields")
                    .to_compile_error();
            }
        },
        _ => unreachable!(),
    };

    let mut field_infos: Vec<FromFieldInfo> = Vec::new();
    for f in fields.iter() {
        let fname = f.ident.as_ref().unwrap().to_string();
        let fdocs = extract_doc_comments(&f.attrs);
        let fdoc = if fdocs.is_empty() { fname.clone() } else { fdocs.join(" ") };
        let (is_option, inner_type) = check_option_type(&f.ty);
        let (is_vec, vec_inner_type, vec_inner_syn_type) = check_vec_type(&f.ty);
        let is_required = has_markdown_required(&f.attrs);
        field_infos.push(FromFieldInfo { name: fname, doc: fdoc, is_option, is_vec, inner_type, vec_inner_type, vec_inner_syn_type, is_required });
    }

    let _field_count = field_infos.len();

    // Generate schema_markdown for struct
    // Name collision detection (case-insensitive)
    {
        let type_lower = struct_name_str.to_lowercase();
        for f in &field_infos {
            if f.name.to_lowercase() == type_lower {
                return syn::Error::new_spanned(
                    struct_name,
                    format!("FromMarkdown: field name '{}' collides with type name '{}' (case-insensitive). This would cause ambiguous separators.", f.name, struct_name_str)
                ).to_compile_error();
            }
        }
    }

    let schema_body = gen_struct_schema_markdown(&struct_name_str, &field_infos);

    // Generate from_markdown for struct
    let parse_body = gen_struct_from_markdown(struct_name, &struct_name_str, &field_infos);

    quote! {
        impl ::mad_hatter::llm::FromMarkdown for #struct_name {
            fn schema_markdown(token: &str) -> ::std::string::String {
                #schema_body
            }
            fn from_markdown(text: &str, token: &str) -> ::std::result::Result<::std::vec::Vec<Self>, ::std::string::String> {
                #parse_body
            }
            fn type_name() -> &'static str {
                #struct_name_str
            }
        }
        impl ::mad_hatter::llm::StructOutput for #struct_name {}
    }
}

/// Generate schema_markdown body for struct.
fn gen_struct_schema_markdown(struct_name_str: &str, fields: &[FromFieldInfo]) -> TokenStream {
    let mut field_lines = Vec::new();
    for f in fields {
        let fname = &f.name;
        let fdoc = &f.doc;
        if f.is_option {
            field_lines.push(quote! {
                __out.push_str(&::std::format!("{fname}-{token}\n{fdoc}（可选）\n", fname = #fname, token = token, fdoc = #fdoc));
            });
        } else {
            field_lines.push(quote! {
                __out.push_str(&::std::format!("{fname}-{token}\n{fdoc}\n", fname = #fname, token = token, fdoc = #fdoc));
            });
        }
    }

    quote! {
        let mut __out = ::std::string::String::new();
        __out.push_str(&::std::format!("每个元素以 {sname}-{token} 开头，包含以下字段：\n",
            sname = #struct_name_str, token = token));
        #(#field_lines)*
        __out.push_str(&::std::format!("所有元素结束后输出: {sname}-end-{token}\n",
            sname = #struct_name_str, token = token));
        __out
    }
}

/// Generate from_markdown body for struct.
fn gen_struct_from_markdown(struct_name: &syn::Ident, struct_name_str: &str, fields: &[FromFieldInfo]) -> TokenStream {
    let field_count = fields.len();

    let mut field_names: Vec<String> = Vec::new();
    let mut field_idents: Vec<syn::Ident> = Vec::new();

    for f in fields {
        field_names.push(f.name.clone());
        field_idents.push(syn::Ident::new(&f.name, proc_macro2::Span::call_site()));
    }

    let sep_name_literals: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();

    // Generate field extraction code (similar to gen_multi_field_parse but for struct)
    let mut field_extractions = Vec::new();
    let mut field_var_names: Vec<syn::Ident> = Vec::new();

    for (i, f) in fields.iter().enumerate() {
        let var_name = syn::Ident::new(&format!("__field_{}", f.name), proc_macro2::Span::call_site());
        let fname = &f.name;

        if f.is_option {
            let inner = &f.inner_type;
            if is_numeric_inner_type(inner) {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __raw.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("[{}] Failed to parse field '{}': {}", #struct_name_str, #fname, e)
                                ),
                            }
                        }
                    };
                });
            } else {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim_end()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(__raw.to_string())
                        }
                    };
                });
            }
        } else {
            let fname_str = &f.name;
            if f.is_required {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                        let __val = if #i == #field_count - 1 {
                            __raw.trim_end().to_string()
                        } else {
                            __raw.trim_end_matches('\n').to_string()
                        };
                        if __val.trim().is_empty() {
                            return ::std::result::Result::Err(
                                ::std::format!("[{}] Required field '{}' is empty", #struct_name_str, #fname_str)
                            );
                        }
                        __val
                    };
                });
            } else {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| *s).unwrap_or("");
                        if #i == #field_count - 1 {
                            __raw.trim_end().to_string()
                        } else {
                            __raw.trim_end_matches('\n').to_string()
                        }
                    };
                });
            }
        }

        field_var_names.push(var_name);
    }

    let constructor_fields: Vec<TokenStream> = field_idents.iter().zip(field_var_names.iter()).map(|(fi, vn)| {
        quote! { #fi: #vn }
    }).collect();

    quote! {
        let __stripped = ::mad_hatter::llm::strip_code_block(text);
        let text = __stripped.as_str();

        let mut __results: ::std::vec::Vec<#struct_name> = ::std::vec::Vec::new();
        let __element_sep = ::std::format!("{}-{}", #struct_name_str, token);
        let __end_marker = ::std::format!("{}-end-{}", #struct_name_str, token);

        let __text_trimmed = text.trim();

        // Check end marker
        if !__text_trimmed.ends_with(&__end_marker) {
            let __tail: ::std::string::String = if __text_trimmed.len() > 200 {
                ::std::format!("...{}", &__text_trimmed[__text_trimmed.len() - 200..])
            } else {
                __text_trimmed.to_string()
            };
            if __text_trimmed.contains(&__element_sep) {
                return ::std::result::Result::Err(
                    ::std::format!("[{}] Missing end marker '{}'. Content tail (up to 200 chars): {}", #struct_name_str, __end_marker, __tail)
                );
            } else {
                return ::std::result::Result::Err(
                    ::std::format!("[{}] No valid element found. Expected '{}' to start output. Content (up to 200 chars): {}", #struct_name_str, __element_sep, __tail)
                );
            }
        }

        // Remove end marker
        let __text_body = __text_trimmed[..__text_trimmed.len() - __end_marker.len()].trim();

        // Split by element separator
        let __sep_with_newline = ::std::format!("{}\n", __element_sep);
        let __chunks: ::std::vec::Vec<&str> = __text_body.split(&__sep_with_newline).collect();

        for __chunk in __chunks {
            let __chunk = __chunk.trim();
            if __chunk.is_empty() { continue; }

            // For struct, there's no variant name line. The chunk directly contains field separators.
            let __body = __chunk;

            let __seps: ::std::vec::Vec<::std::string::String> = [#(#sep_name_literals),*]
                .iter()
                .map(|n| ::std::format!("{}-{}", n, token))
                .collect();

            let mut __field_values: ::std::vec::Vec<&str> = ::std::vec::Vec::new();
            let mut __remaining = __body;

            for (i, sep) in __seps.iter().enumerate() {
                let __sep_with_newline = ::std::format!("{}\n", sep);
                match __remaining.find(&__sep_with_newline) {
                    Some(pos) => {
                        __remaining = &__remaining[pos + __sep_with_newline.len()..];
                    }
                    None => {
                        if __remaining.trim_end() == sep.as_str() {
                            __remaining = "";
                        } else {
                            __field_values.push("");
                            continue;
                        }
                    }
                }

                let mut __end = __remaining.len();
                for next_sep in __seps.iter().skip(i + 1) {
                    let __next_sep_with_newline = ::std::format!("{}\n", next_sep);
                    if let Some(pos) = __remaining.find(&__next_sep_with_newline) {
                        __end = pos;
                        break;
                    }
                    let __trimmed = __remaining.trim_end();
                    if __trimmed.ends_with(next_sep.as_str()) {
                        __end = __trimmed.len() - next_sep.len();
                        break;
                    }
                }

                __field_values.push(&__remaining[..__end]);
                __remaining = &__remaining[__end..];
            }

            #(#field_extractions)*

            __results.push(#struct_name {
                #(#constructor_fields),*
            });
        }

        ::std::result::Result::Ok(__results)
    }
}

// ============================================================
// Shared helpers
// ============================================================

/// Convert CamelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Check if a type is `Option<T>` and return (is_option, inner_type_name).
fn check_option_type(ty: &syn::Type) -> (bool, String) {
    if let syn::Type::Path(type_path) = ty {
        let last_seg = type_path.path.segments.last();
        if let Some(seg) = last_seg {
            if seg.ident == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        let inner_str = quote!(#inner_ty).to_string().replace(' ', "");
                        return (true, inner_str);
                    }
                }
            }
        }
    }
    (false, String::new())
}

/// Check if a type is `Vec<T>` and return (is_vec, inner_type_name, inner_syn_type).
fn check_vec_type(ty: &syn::Type) -> (bool, String, Option<syn::Type>) {
    if let syn::Type::Path(type_path) = ty {
        let last_seg = type_path.path.segments.last();
        if let Some(seg) = last_seg {
            if seg.ident == "Vec" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() {
                        let inner_str = quote!(#inner_ty).to_string().replace(' ', "");
                        return (true, inner_str, Some(inner_ty.clone()));
                    }
                }
            }
        }
    }
    (false, String::new(), None)
}

/// Check if an inner type name is a numeric type (for Option<numeric> parsing).
fn is_numeric_inner_type(inner: &str) -> bool {
    matches!(inner, "u8" | "u16" | "u32" | "u64" | "usize" |
                    "i8" | "i16" | "i32" | "i64" | "isize" |
                    "f32" | "f64")
}

/// Extract doc comments from attributes.
fn extract_doc_comments(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut docs = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &nv.value {
                    if let Lit::Str(s) = &expr_lit.lit {
                        docs.push(s.value().trim().to_string());
                    }
                }
            }
        }
    }
    docs
}

/// Check if a field has `#[markdown(skip)]` attribute.
fn has_markdown_skip(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("markdown") {
            if let Ok(nested) = attr.parse_args::<syn::Ident>() {
                if nested == "skip" {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a field has `#[markdown(required)]` attribute.
fn has_markdown_required(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("markdown") {
            if let Ok(nested) = attr.parse_args::<syn::Ident>() {
                if nested == "required" {
                    return true;
                }
            }
        }
    }
    false
}