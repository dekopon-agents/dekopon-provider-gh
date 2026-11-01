use dekopon_provider_sdk::{
    EffectKind, RiskLevel,
    provider::{Capability, Code, Failure, Http, Proposal, Provider, Request, Stdout, Usage},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct Fence;
#[derive(clap::Parser)]
struct Args {}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    uri: String,
    content_type: String,
    body: String,
}
impl Provider for Fence {
    const ID: &'static str = "gh";
    const DESCRIPTION: &'static str = "Isolated raw-request policy fixture";
    const COMMAND_WORDS: &'static [&'static str] = &["fence"];
    type Args = Args;
    type Capabilities = (Write,);
    fn propose(_: Args, _: bool) -> Result<Proposal<Self>, Usage> {
        Ok(Proposal::to::<Write>(Input {
            uri: "http://127.0.0.1/".into(),
            content_type: "application/json".into(),
            body: String::new(),
        }))
    }
}
struct Write;
#[derive(Debug)]
struct Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixture request refused")
    }
}
impl Failure for Error {
    fn code(&self) -> Code {
        Code::new("fixture-refused")
    }
}
impl Capability for Write {
    type Provider = Fence;
    const NAME: &'static str = "content.write";
    const DESCRIPTION: &'static str = "Exercise the existing broker request fence";
    const EFFECT: EffectKind = EffectKind::ExternalWrite;
    const RISK: RiskLevel = RiskLevel::Medium;
    type Input = Input;
    type Needs = Http;
    type Error = Error;
    fn run(input: Input, http: Http, _: &mut Stdout) -> Result<(), Error> {
        let request = Request::new("PUT", input.uri)
            .map_err(|_| Error)?
            .with_header(
                dekopon_provider_sdk::provider::Header::text("content-type", &input.content_type)
                    .map_err(|_| Error)?,
            )
            .with_header(
                dekopon_provider_sdk::provider::Header::text(
                    "accept",
                    "application/vnd.github+json",
                )
                .map_err(|_| Error)?,
            )
            .with_body(input.body.into_bytes());
        http.send(request).map_err(|_| Error)?;
        Ok(())
    }
}
dekopon_provider_sdk::export!(Fence);
