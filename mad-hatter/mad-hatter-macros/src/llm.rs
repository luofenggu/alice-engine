use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Lit, Meta};

// ============================================================
// Field type classification (for ToMarkdown)
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
    section_title: String,
    kind: FieldKind,
}

/// Generate `impl ToMarkdown for Struct` from derive attributes.
pub fn derive_to_markdown(input: DeriveInput) -> TokenStream {
    let struct_name = &input.ident;

    // Extract struct-level doc comments
    let struct_docs = extract_doc_comments(&input.attrs);

    // Ensure it's a struct with named fields
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(&input.ident, "ToMarkdown only supports structs with named fields")
                    .to_compile_error();
            }
        },
        _ => {
            return syn::Error::new_spanned(&input.ident, "ToMarkdown only supports structs")
                .to_compile_error();
        }
    };

    // Collect field info
    let mut tm_fields: Vec<ToMarkdownField> = Vec::new();

    for field in fields {
        let field_ident = field.ident.as_ref().unwrap().clone();

        if has_markdown_skip(&field.attrs) {
            continue;
        }

        let field_docs = extract_doc_comments(&field.attrs);
        let section_title = if field_docs.is_empty() {
            field_ident.to_string()
        } else {
            field_docs.join("\n")
        };

        let kind = classify_field_type(&field.ty);

        tm_fields.push(ToMarkdownField {
            ident: field_ident,
            section_title,
            kind,
        });
    }

    // Generate to_markdown_depth field renders
    let depth_renders: Vec<TokenStream> = tm_fields.iter().map(|f| {
        gen_depth_render(f)
    }).collect();

    // Generate to_markdown_item field renders
    let item_renders: Vec<TokenStream> = tm_fields.iter().map(|f| {
        gen_item_render(f)
    }).collect();

    // Build the header from struct-level doc comments
    let header = if struct_docs.is_empty() {
        quote! {}
    } else {
        let header_text = struct_docs.join("\n");
        quote! {
            __out.push_str(#header_text);
            __out.push('\n');
        }
    };

    quote! {
        impl ::mad_hatter::llm::ToMarkdown for #struct_name {
            fn to_markdown_depth(&self, __depth: usize) -> ::std::string::String {
                let mut __out = ::std::string::String::new();
                #header
                #(#depth_renders)*
                __out
            }

            fn to_markdown_item(&self) -> ::std::string::String {
                let mut __out = ::std::string::String::new();
                #(#item_renders)*
                __out
            }
        }
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
                    let __val: &str = self.#ident.as_ref();
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
                        __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                        for (__i, __item) in __items.iter().enumerate() {
                            if __i > 0 {
                                __out.push('\n');
                            }
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
                    __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_depth(&self.#ident, __depth + 1));
                }
            }
        }
    }
}

/// Generate code to render a single Vec element in depth mode.
fn gen_vec_element_render_depth(inner_kind: &FieldKind) -> TokenStream {
    match inner_kind {
        FieldKind::String => {
            quote! {
                __out.push_str(__item.as_str());
                __out.push('\n');
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                __out.push_str(&__item.to_string());
                __out.push('\n');
            }
        }
        _ => {
            // Struct or other complex type: use to_markdown_item for compact format
            quote! {
                __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_item(__item));
            }
        }
    }
}

/// Generate depth-mode rendering for Option<T> fields.
fn gen_option_depth_render(ident: &syn::Ident, title: &str, inner_kind: &FieldKind) -> TokenStream {
    let value_render = match inner_kind {
        FieldKind::String => {
            quote! {
                let __s: &str = __val.as_ref();
                if !__s.is_empty() {
                    let __hashes: ::std::string::String = "#".repeat(__depth + 1);
                    __out.push_str(&::std::format!("\n{} {} {}\n", __hashes, #title, __hashes));
                    __out.push_str(__s);
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
                __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_depth(__val, __depth + 1));
            }
        }
    };

    quote! {
        {
            if let ::std::option::Option::Some(__val) = &self.#ident {
                #value_render
            }
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
                    let __val: &str = self.#ident.as_ref();
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
            // Nested struct in item mode: render its item form
            quote! {
                {
                    __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_item(&self.#ident));
                }
            }
        }
    }
}

/// Generate code to render a single Vec element in item mode.
fn gen_vec_element_render_item(inner_kind: &FieldKind) -> TokenStream {
    match inner_kind {
        FieldKind::String => {
            quote! {
                __out.push_str(__item.as_str());
                __out.push('\n');
            }
        }
        FieldKind::Bool | FieldKind::Numeric => {
            quote! {
                __out.push_str(&__item.to_string());
                __out.push('\n');
            }
        }
        _ => {
            quote! {
                __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_item(__item));
                __out.push('\n');
            }
        }
    }
}

/// Generate item-mode rendering for Option<T> fields.
fn gen_option_item_render(ident: &syn::Ident, field_name: &str, inner_kind: &FieldKind) -> TokenStream {
    let value_render = match inner_kind {
        FieldKind::String => {
            quote! {
                let __s: &str = __val.as_ref();
                if !__s.is_empty() {
                    __out.push_str(#field_name);
                    __out.push_str(": ");
                    __out.push_str(__s);
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
                __out.push_str(&::mad_hatter::llm::ToMarkdown::to_markdown_item(__val));
                __out.push('\n');
            }
        }
    };

    quote! {
        {
            if let ::std::option::Option::Some(__val) = &self.#ident {
                #value_render
            }
        }
    }
}

// ============================================================
// FromMarkdown derive for enums
// ============================================================

/// Information about a single enum variant, extracted at macro time.
struct VariantInfo {
    ident: syn::Ident,
    snake_name: String,
    doc: String,
    fields: Vec<FieldInfo>,
}

/// Information about a single field within a variant.
struct FieldInfo {
    name: String,
    doc: String,
    is_option: bool,
    /// The inner type name for Option fields (e.g. "u64", "String")
    inner_type: String,
}

/// Generate `impl FromMarkdown for Enum`.
pub fn derive_from_markdown(input: DeriveInput) -> TokenStream {
    let enum_name = &input.ident;
    let enum_name_str = enum_name.to_string();

    let variants_data = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new_spanned(&input.ident, "FromMarkdown only supports enums")
                .to_compile_error();
        }
    };

    // Extract variant info
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
                    FieldInfo { name: fname, doc: fdoc, is_option, inner_type }
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
        }
    }
}

/// Generate the body of `schema_markdown`.
fn gen_schema_markdown(enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    let mut variant_schemas = Vec::new();

    for vi in variants {
        let snake = &vi.snake_name;
        let doc = &vi.doc;
        let required_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| !f.is_option).collect();
        let option_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| f.is_option).collect();
        let total = vi.fields.len();

        if total == 0 {
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake
                ));
            });
        } else if total == 1 && option_fields.len() == 1 {
            let fdoc = &option_fields[0].doc;
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n第三行可选输出: {fdoc}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                ));
            });
        } else if total == 1 && required_fields.len() == 1 {
            let fdoc = &required_fields[0].doc;
            variant_schemas.push(quote! {
                __out.push_str(&::std::format!(
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n第三行开始多行输出{fdoc}\n",
                    doc = #doc, ename = #enum_name_str, token = token, snake = #snake, fdoc = #fdoc
                ));
            });
        } else {
            let mut field_lines = Vec::new();
            for f in &vi.fields {
                let fname = &f.name;
                let fdoc = &f.doc;
                if f.is_option {
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
                    "action {doc}\n首行输出: {ename}-{token}\n第二行输出: {snake}\n",
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

/// Generate the body of `from_markdown`.
fn gen_from_markdown(enum_name: &syn::Ident, enum_name_str: &str, variants: &[VariantInfo]) -> TokenStream {
    let mut match_arms = Vec::new();

    for vi in variants {
        let snake = &vi.snake_name;
        let variant_ident = &vi.ident;
        let total = vi.fields.len();
        let required_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| !f.is_option).collect();
        let option_fields: Vec<&FieldInfo> = vi.fields.iter().filter(|f| f.is_option).collect();

        if total == 0 {
            match_arms.push(quote! {
                #snake => {
                    __results.push(#enum_name::#variant_ident);
                }
            });
        } else if total == 1 && option_fields.len() == 1 {
            let field_name_ident = syn::Ident::new(&option_fields[0].name, proc_macro2::Span::call_site());
            let inner = &option_fields[0].inner_type;

            if inner == "u64" || inner == "i64" || inner == "u32" || inner == "i32" || inner == "usize" {
                match_arms.push(quote! {
                    #snake => {
                        let __rest = __body.trim();
                        let __val = if __rest.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __rest.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("Failed to parse field '{}' of variant '{}': {}", #snake, stringify!(#variant_ident), e)
                                ),
                            }
                        };
                        __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                    }
                });
            } else {
                match_arms.push(quote! {
                    #snake => {
                        let __rest = __body.trim_end();
                        let __val = if __rest.is_empty() {
                            ::std::option::Option::None
                        } else {
                            ::std::option::Option::Some(__rest.to_string())
                        };
                        __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                    }
                });
            }
        } else if total == 1 && required_fields.len() == 1 {
            let field_name_ident = syn::Ident::new(&required_fields[0].name, proc_macro2::Span::call_site());
            match_arms.push(quote! {
                #snake => {
                    let __val = __body.trim_end().to_string();
                    __results.push(#enum_name::#variant_ident { #field_name_ident: __val });
                }
            });
        } else {
            let multi_parse = gen_multi_field_parse(enum_name, vi);
            match_arms.push(quote! {
                #snake => {
                    #multi_parse
                }
            });
        }
    }

    quote! {
        let mut __results: ::std::vec::Vec<#enum_name> = ::std::vec::Vec::new();
        let __separator = ::std::format!("{}-{}", #enum_name_str, token);
        let __end_marker = ::std::format!("{}-end-{}", #enum_name_str, token);

        let __trimmed = text.trim_end();
        if !__trimmed.ends_with(&__end_marker) {
            return ::std::result::Result::Err(
                ::std::format!("Output truncated: missing end marker {}", __end_marker)
            );
        }

        let __text_without_end = &__trimmed[..__trimmed.len() - __end_marker.len()];

        let __blocks: ::std::vec::Vec<&str> = __text_without_end.split(&__separator).collect();

        for __block in __blocks.iter().skip(1) {
            let __block = if __block.starts_with('\n') { &__block[1..] } else { *__block };

            if __block.trim().is_empty() {
                continue;
            }

            let (__variant_line, __body) = match __block.find('\n') {
                Some(pos) => (__block[..pos].trim(), &__block[pos + 1..]),
                None => (__block.trim(), ""),
            };

            match __variant_line {
                #(#match_arms)*
                __unknown => {
                    return ::std::result::Result::Err(
                        ::std::format!("Unknown variant: '{}'", __unknown)
                    );
                }
            }
        }

        ::std::result::Result::Ok(__results)
    }
}

/// Generate parsing code for a variant with N>=2 fields.
fn gen_multi_field_parse(enum_name: &syn::Ident, vi: &VariantInfo) -> TokenStream {
    let variant_ident = &vi.ident;
    let snake = &vi.snake_name;

    let mut field_names: Vec<String> = Vec::new();
    let mut field_idents: Vec<syn::Ident> = Vec::new();
    let mut field_is_option: Vec<bool> = Vec::new();
    let mut field_inner_types: Vec<String> = Vec::new();

    for f in &vi.fields {
        field_names.push(f.name.clone());
        field_idents.push(syn::Ident::new(&f.name, proc_macro2::Span::call_site()));
        field_is_option.push(f.is_option);
        field_inner_types.push(f.inner_type.clone());
    }

    let field_count = field_names.len();
    let sep_name_literals: Vec<&str> = field_names.iter().map(|s| s.as_str()).collect();

    let mut field_extractions = Vec::new();
    let mut field_var_names: Vec<syn::Ident> = Vec::new();

    for (i, f) in vi.fields.iter().enumerate() {
        let var_name = syn::Ident::new(&format!("__field_{}", f.name), proc_macro2::Span::call_site());
        let fname = &f.name;

        if f.is_option {
            let inner = &f.inner_type;
            if inner == "u64" || inner == "i64" || inner == "u32" || inner == "i32" || inner == "usize" {
                field_extractions.push(quote! {
                    let #var_name = {
                        let __raw = __field_values.get(#i).map(|s| s.trim()).unwrap_or("");
                        if __raw.is_empty() {
                            ::std::option::Option::None
                        } else {
                            match __raw.parse() {
                                Ok(v) => ::std::option::Option::Some(v),
                                Err(e) => return ::std::result::Result::Err(
                                    ::std::format!("Failed to parse field '{}' of variant '{}': {}", #fname, #snake, e)
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