use {
    proc_macro::TokenStream,
    quote::quote,
    semver::Version,
};

#[proc_macro_attribute]
pub fn current_version(_: TokenStream, item: TokenStream) -> TokenStream {
    let version = Version::parse(env!("CARGO_PKG_VERSION")).expect("failed to parse package version");
    let version_endpoint = format!("/v{}", version.major);
    let item = proc_macro2::TokenStream::from(item);
    TokenStream::from(quote! {
        #[rocket::get(#version_endpoint)]
        #item
    })
}
