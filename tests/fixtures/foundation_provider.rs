use dekopon_gh_provider::{
    error::ProviderError,
    graphql::{self, RepositoryVariables},
};
use dekopon_provider_sdk::provider::{
    Capability, Http, Proposal, Provider, Settings, Stdout, Usage,
};
use dekopon_provider_sdk::{EffectKind, RiskLevel};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::Write;

pub struct Foundation;

#[derive(clap::Parser)]
pub struct Args {}

impl Provider for Foundation {
    const ID: &'static str = "gh";
    const COMMAND_WORDS: &'static [&'static str] = &["foundation"];
    const DESCRIPTION: &'static str = "Test fixture for the production GraphQL helper";
    type Args = Args;
    type Capabilities = (Repository,);
    fn propose(_: Args, _: bool) -> Result<Proposal<Self>, Usage> {
        Ok(Proposal::to::<Repository>(Input {
            owner: "octo".into(),
            name: "hello".into(),
        }))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    owner: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureSettings {
    base_url: dekopon_provider_sdk::provider::endpoint::Base,
}

pub struct Repository;

impl Capability for Repository {
    type Provider = Foundation;
    const NAME: &'static str = "foundation.read-repository-node";
    const DESCRIPTION: &'static str = "Exercise a fixed repository query";
    const EFFECT: EffectKind = EffectKind::ReadOnly;
    const RISK: RiskLevel = RiskLevel::Low;
    type Input = Input;
    type Needs = (Settings<FixtureSettings>, Http);
    type Error = ProviderError;
    fn run(
        input: Input,
        (settings, http): Self::Needs,
        out: &mut Stdout,
    ) -> Result<(), Self::Error> {
        let result = graphql::repository_node(
            RepositoryVariables {
                owner: input.owner,
                name: input.name,
            },
            &settings.into_inner().base_url,
            &mut |request| http.send(request),
        )?;
        writeln!(
            out,
            "{}",
            serde_json::to_string(&result).expect("serializable node")
        )
        .expect("fixture output");
        Ok(())
    }
}
