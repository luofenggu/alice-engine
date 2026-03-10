use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Ident, LitStr, Token, Type, Result,
};

/// Top-level: `service Name { ... }`
pub struct ServiceDef {
    pub name: Ident,
    pub endpoints: Vec<EndpointDef>,
}

/// HTTP method
#[derive(Debug, Clone)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

/// A single endpoint: `METHOD "path" => fn_name(params) -> RetType;`
pub struct EndpointDef {
    pub method: HttpMethod,
    pub path: String,
    pub fn_name: Ident,
    pub params: Vec<ParamDef>,
    pub return_type: Option<Type>,
}

/// A parameter: `name: Type`
pub struct ParamDef {
    pub name: Ident,
    pub ty: Type,
}

impl Parse for ServiceDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse `service`
        let service_kw: Ident = input.parse()?;
        if service_kw != "service" {
            return Err(syn::Error::new(service_kw.span(), "expected `service`"));
        }

        let name: Ident = input.parse()?;

        let content;
        syn::braced!(content in input);

        let mut endpoints = Vec::new();
        while !content.is_empty() {
            endpoints.push(content.parse::<EndpointDef>()?);
        }

        Ok(ServiceDef { name, endpoints })
    }
}

impl Parse for EndpointDef {
    fn parse(input: ParseStream) -> Result<Self> {
        // Parse HTTP method (GET, POST, PUT, DELETE, PATCH)
        let method_ident: Ident = input.parse()?;
        let method = match method_ident.to_string().as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "DELETE" => HttpMethod::Delete,
            "PATCH" => HttpMethod::Patch,
            other => {
                return Err(syn::Error::new(
                    method_ident.span(),
                    format!("unknown HTTP method: {}", other),
                ));
            }
        };

        // Parse path string
        let path_lit: LitStr = input.parse()?;
        let path = path_lit.value();

        // Parse `=>`
        input.parse::<Token![=>]>()?;

        // Parse function name
        let fn_name: Ident = input.parse()?;

        // Parse parameters in parentheses
        let params_content;
        syn::parenthesized!(params_content in input);
        let params_punctuated: Punctuated<ParamDef, Token![,]> =
            Punctuated::parse_terminated(&params_content)?;
        let params: Vec<ParamDef> = params_punctuated.into_iter().collect();

        // Parse optional return type: `-> Type`
        let return_type = if input.peek(Token![->]) {
            input.parse::<Token![->]>()?;
            Some(input.parse::<Type>()?)
        } else {
            None
        };

        // Parse trailing semicolon
        input.parse::<Token![;]>()?;

        Ok(EndpointDef {
            method,
            path,
            fn_name,
            params,
            return_type,
        })
    }
}

impl Parse for ParamDef {
    fn parse(input: ParseStream) -> Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: Type = input.parse()?;
        Ok(ParamDef { name, ty })
    }
}
