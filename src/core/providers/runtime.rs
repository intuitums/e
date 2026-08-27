//! Provider runtime assembly.
//!
//! Registry data chooses a provider, the catalog chooses a model, this layer
//! resolves or refreshes credentials, and an API module only translates the
//! normalized request and event stream for one wire dialect.

use crate::core::auth::{self, login};
use crate::core::providers::catalog::{Api, Model};
use crate::core::providers::registry::ResponsesMount;
use crate::core::providers::ProviderError;

#[derive(Clone, Debug)]
pub struct Authorization {
    pub bearer: String,
    /// Present only for the ChatGPT subscription mount of Responses.
    pub account_id: Option<String>,
    /// False for an unkeyed local backend. Request dialects historically
    /// send a harmless placeholder bearer; catalog probes omit it.
    pub credentialed: bool,
}

pub async fn authorize(model: &Model) -> Result<Authorization, ProviderError> {
    authorize_provider(&model.provider, model.api, model.responses_mount).await
}

pub async fn authorize_provider(
    provider: &str,
    api: Api,
    responses_mount: ResponsesMount,
) -> Result<Authorization, ProviderError> {
    // Mount selection is deployment data, never inferred from whichever
    // credential representation happens to be stored for the provider.
    if api == Api::Responses && responses_mount == ResponsesMount::Codex {
        let (bearer, account_id) = login::codex_access(provider)
            .await
            .map_err(ProviderError::auth)?;
        return Ok(Authorization {
            bearer,
            account_id: Some(account_id),
            credentialed: true,
        });
    }

    let loaded = auth::load();
    let credentialed = loaded.contains_key(provider)
        || !crate::core::providers::registry::find(provider).is_some_and(|entry| entry.auth.none);
    let bearer = login::access_token(provider)
        .await
        .map_err(ProviderError::auth)?;
    Ok(Authorization {
        bearer,
        account_id: None,
        credentialed,
    })
}
