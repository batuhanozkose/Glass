use anyhow::Result;
use gpui::{App, AppContext, Entity, SharedString, Window};
use serde::Deserialize;
use service_hub::{
    ServiceAuthAction, ServiceAuthActionDescriptor, ServiceAuthActionRequest,
    ServiceInputDescriptor, ServiceProviderDescriptor,
};
use ui::ToggleState;
use ui_input::InputField;

use crate::command_runner::run_json_operation;

#[derive(Clone, Debug)]
pub(crate) struct AscAuthSummary {
    pub headline: String,
    pub detail: String,
    pub warnings: Vec<String>,
    pub healthy: bool,
    pub authenticated: bool,
}

pub(crate) struct ServiceAuthFormState {
    authenticate_descriptor: Option<ServiceAuthActionDescriptor>,
    pub(crate) fields: Vec<ServiceAuthFieldState>,
    pub(crate) expanded: bool,
    pub(crate) pending: bool,
    pub(crate) error_message: Option<SharedString>,
    pub(crate) logout_available: bool,
}

pub(crate) enum ServiceAuthFieldState {
    Text {
        descriptor: ServiceInputDescriptor,
        input: Entity<InputField>,
    },
    Toggle {
        descriptor: ServiceInputDescriptor,
        value: ToggleState,
    },
}

impl ServiceAuthFormState {
    pub(crate) fn new(
        provider: &ServiceProviderDescriptor,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let authenticate_descriptor = provider.auth.as_ref().and_then(|auth| {
            auth.actions
                .iter()
                .find(|action| action.action == ServiceAuthAction::Authenticate)
                .cloned()
        });
        let logout_available = provider
            .auth
            .as_ref()
            .is_some_and(|auth| {
                auth.actions
                    .iter()
                    .any(|action| action.action == ServiceAuthAction::Logout)
            });

        Self {
            fields: authenticate_descriptor
                .as_ref()
                .map(|descriptor| create_fields(&descriptor.inputs, window, cx))
                .unwrap_or_default(),
            authenticate_descriptor,
            expanded: false,
            pending: false,
            error_message: None,
            logout_available,
        }
    }

    pub(crate) fn show(&mut self) {
        self.expanded = true;
        self.error_message = None;
    }

    pub(crate) fn finish_success(&mut self) {
        self.expanded = false;
        self.pending = false;
        self.error_message = None;
    }

    pub(crate) fn cancel(&mut self) {
        self.expanded = false;
        self.pending = false;
        self.error_message = None;
    }

    pub(crate) fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    pub(crate) fn set_error(&mut self, error: impl Into<SharedString>) {
        self.error_message = Some(error.into());
    }

    pub(crate) fn set_toggle(&mut self, key: &str, value: ToggleState) {
        for field in &mut self.fields {
            let ServiceAuthFieldState::Toggle {
                descriptor,
                value: current,
            } = field
            else {
                continue;
            };
            if descriptor.key == key {
                *current = value;
            }
        }
    }

    pub(crate) fn set_text(
        &self,
        key: &str,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        for field in &self.fields {
            let ServiceAuthFieldState::Text { descriptor, input } = field else {
                continue;
            };
            if descriptor.key == key {
                input.update(cx, |input, cx| {
                    input.set_text(text, window, cx);
                });
            }
        }
    }

    pub(crate) fn build_authenticate_request(
        &self,
        provider_id: &str,
        cx: &App,
    ) -> Result<ServiceAuthActionRequest, SharedString> {
        let Some(descriptor) = self.authenticate_descriptor.as_ref() else {
            return Err("Authentication is not available for this provider".into());
        };

        let mut input = std::collections::BTreeMap::new();
        for field in &self.fields {
            match field {
                ServiceAuthFieldState::Text { descriptor, input: editor } => {
                    let value = editor.read(cx).text(cx).trim().to_string();
                    if descriptor.required && value.is_empty() {
                        return Err(format!("{} is required", descriptor.label).into());
                    }
                    if !value.is_empty() {
                        input.insert(descriptor.key.clone(), value);
                    }
                }
                ServiceAuthFieldState::Toggle { descriptor, value } => {
                    input.insert(
                        descriptor.key.clone(),
                        (value.selected()).to_string(),
                    );
                }
            }
        }

        Ok(ServiceAuthActionRequest {
            provider_id: provider_id.to_string(),
            action: descriptor.action,
            input,
        })
    }

    pub(crate) fn build_logout_request(
        &self,
        provider_id: &str,
    ) -> Option<ServiceAuthActionRequest> {
        self.logout_available.then(|| ServiceAuthActionRequest {
            provider_id: provider_id.to_string(),
            action: ServiceAuthAction::Logout,
            input: Default::default(),
        })
    }
}

fn create_fields(
    descriptors: &[ServiceInputDescriptor],
    window: &mut Window,
    cx: &mut App,
) -> Vec<ServiceAuthFieldState> {
    descriptors
        .iter()
        .enumerate()
        .map(|(index, descriptor)| match descriptor.kind {
            service_hub::ServiceInputKind::Text | service_hub::ServiceInputKind::FilePath => {
                let input = cx.new(|cx| {
                    InputField::new(
                        window,
                        cx,
                        descriptor.placeholder.as_deref().unwrap_or_default(),
                    )
                    .label(descriptor.label.clone())
                    .tab_index(index as isize + 1)
                    .tab_stop(true)
                });
                ServiceAuthFieldState::Text {
                    descriptor: descriptor.clone(),
                    input,
                }
            }
            service_hub::ServiceInputKind::Toggle => ServiceAuthFieldState::Toggle {
                descriptor: descriptor.clone(),
                value: ToggleState::Unselected,
            },
        })
        .collect()
}

#[derive(Deserialize)]
struct AscAuthStatusResponse {
    #[serde(rename = "storageBackend")]
    storage_backend: String,
    warnings: Option<Vec<String>>,
    credentials: Vec<AscCredential>,
    #[serde(rename = "environmentNote")]
    environment_note: Option<String>,
}

#[derive(Deserialize)]
struct AscCredential {
    name: String,
    #[serde(rename = "isDefault")]
    is_default: bool,
    validation: Option<String>,
    #[serde(rename = "validationDetail")]
    validation_detail: Option<String>,
    #[serde(rename = "validationError")]
    validation_error: Option<String>,
}

pub(crate) async fn load_auth_status() -> Result<AscAuthSummary> {
    let response: AscAuthStatusResponse = run_json_operation(service_hub::ServiceOperationRequest {
        provider_id: "app-store-connect".to_string(),
        operation: "auth_status".to_string(),
        resource: None,
        artifact: None,
        input: Default::default(),
    })
    .await?;

    Ok(summarize_auth_status(response))
}

fn summarize_auth_status(response: AscAuthStatusResponse) -> AscAuthSummary {
    let mut warnings = response.warnings.unwrap_or_default();
    if let Some(note) = response.environment_note.filter(|note| !note.trim().is_empty()) {
        warnings.push(note);
    }

    let default_credential = response
        .credentials
        .iter()
        .find(|credential| credential.is_default)
        .or_else(|| response.credentials.first());

    let Some(default_credential) = default_credential else {
        return AscAuthSummary {
            headline: "Not authenticated".to_string(),
            detail: format!(
                "No App Store Connect credentials are stored in {}.",
                response.storage_backend
            ),
            warnings,
            healthy: false,
            authenticated: false,
        };
    };

    let validation = default_credential.validation.as_deref().unwrap_or("unknown");
    let healthy = validation.eq_ignore_ascii_case("works");
    let detail = default_credential
        .validation_error
        .clone()
        .or_else(|| default_credential.validation_detail.clone())
        .unwrap_or_else(|| {
            format!(
                "Default credential: {} in {}",
                default_credential.name, response.storage_backend
            )
        });

    AscAuthSummary {
        headline: if healthy {
            "Authentication validated".to_string()
        } else {
            format!("Authentication status: {validation}")
        },
        detail,
        warnings,
        healthy,
        authenticated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{AscAuthStatusResponse, AscCredential, summarize_auth_status};

    #[test]
    fn summarizes_missing_credentials_as_not_authenticated() {
        let summary = summarize_auth_status(AscAuthStatusResponse {
            storage_backend: "System Keychain".to_string(),
            warnings: None,
            credentials: Vec::new(),
            environment_note: None,
        });

        assert!(!summary.authenticated);
        assert!(!summary.healthy);
        assert_eq!(summary.headline, "Not authenticated");
    }

    #[test]
    fn summarizes_validated_credentials_as_healthy() {
        let summary = summarize_auth_status(AscAuthStatusResponse {
            storage_backend: "System Keychain".to_string(),
            warnings: Some(vec!["warning".to_string()]),
            credentials: vec![AscCredential {
                name: "Personal".to_string(),
                is_default: true,
                validation: Some("works".to_string()),
                validation_detail: None,
                validation_error: None,
            }],
            environment_note: Some("environment note".to_string()),
        });

        assert!(summary.authenticated);
        assert!(summary.healthy);
        assert_eq!(summary.headline, "Authentication validated");
        assert_eq!(summary.warnings.len(), 2);
    }
}
