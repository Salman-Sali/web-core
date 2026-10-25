use aws_config::SdkConfig;
use aws_sdk_sesv2::{
    Client,
    error::DisplayErrorContext,
    types::{Body, Content, Destination, EmailContent, Message},
};

use crate::{
    config::Config,
    error::{BackupError, Result},
};

pub(crate) async fn send(
    sdk: &SdkConfig,
    cfg: &Config,
    subject: String,
    text: String,
) -> Result<()> {
    let Some(notify) = &cfg.notify else {
        return Ok(());
    };
    let content = |data: String| {
        Content::builder()
            .data(data)
            .charset("UTF-8")
            .build()
            .map_err(|e| BackupError::Email(e.to_string()))
    };
    let message = Message::builder()
        .subject(content(subject)?)
        .body(Body::builder().text(content(text)?).build())
        .build();

    Client::new(sdk)
        .send_email()
        .from_email_address(&notify.from)
        .destination(
            Destination::builder()
                .set_to_addresses(Some(notify.to.clone()))
                .build(),
        )
        .content(EmailContent::builder().simple(message).build())
        .send()
        .await
        .map_err(|e| BackupError::Email(DisplayErrorContext(e).to_string()))?;
    Ok(())
}

/// Sends a notification, logging instead of failing when the email itself cannot be sent.
pub(crate) async fn send_or_log(sdk: &SdkConfig, cfg: &Config, subject: String, text: String) {
    if let Err(e) = send(sdk, cfg, subject, text).await {
        eprintln!("backup: could not send notification email: {e}");
    }
}
