use crate::{error::Error, something_went_wrong};
use aws_sdk_sesv2::{
    Client,
    types::{Body, Content, Destination, EmailContent, Message},
};

const UTF_8: &str = "UTF-8";

pub async fn send_html_mail(
    ses_client: &Client,
    from_email: &str,
    to_email: String,
    subject: String,
    html_body: String,
) -> Result<(), Error> {
    let subject = Content::builder()
        .data(subject)
        .charset(UTF_8)
        .build()
        .map_err(|e| something_went_wrong!("Error while building mail subject {e:?}"))?;

    let body = Body::builder()
        .html(
            Content::builder()
                .data(html_body)
                .charset(UTF_8)
                .build()
                .map_err(|e| something_went_wrong!("Error while building mail body {e:?}"))?,
        )
        .text(
            Content::builder()
                .data(String::from("Is this mail not rendering?"))
                .charset(UTF_8)
                .build()
                .map_err(|e| something_went_wrong!("Error while building mail body {e:?}"))?,
        )
        .build();

    let message = Message::builder().subject(subject).body(body).build();

    let destination = Destination::builder().to_addresses(to_email).build();

    let email_content = EmailContent::builder().simple(message).build();

    ses_client
        .send_email()
        .from_email_address(from_email)
        .destination(destination)
        .content(email_content)
        .send()
        .await
        .map_err(|e| something_went_wrong!("Error while sending email {e:?}"))?;

    return Ok(());
}
