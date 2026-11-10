// SPDX-License-Identifier: MPL-2.0

//! Rooted image-path smoke test: upload local bytes, deliver a safely quoted
//! managed path to the ordinary shell, and terminate the created session.

mod support;

use koru_api_client::sessions::{
    AttachmentId, DeliveryMode, IdempotencyKey, SendPendingInputRequest,
};

fn main() -> support::ExampleResult<()> {
    let args = support::args(true)?;
    let image = args.image.expect("the parser requires --image");
    let client = support::connect(&args.root)?;
    let (session_id, lease) = support::create_and_attach(&client)?;
    let operation = (|| {
        let attachment =
            client
                .sessions()
                .upload_image_path(session_id, lease, AttachmentId::mint(), image)?;
        let delivered = client.sessions().send_pending_input(
            session_id,
            &SendPendingInputRequest {
                idempotency_key: IdempotencyKey::mint(),
                controller_lease: lease,
                text: "echo PUBLIC_IMAGE_PATH_SMOKE".into(),
                attachment_ids: vec![attachment.id],
                references: Vec::new(),
            },
        )?;
        let outcome = delivered
            .attachments
            .iter()
            .find(|result| result.attachment_id == attachment.id)
            .ok_or("delivery response omitted the attachment")?;
        if outcome.mode != DeliveryMode::QuotedPath {
            return Err(format!("expected quoted-path delivery, got {:?}", outcome.mode).into());
        }
        println!(
            "uploaded {} bytes and delivered a safely quoted managed path",
            attachment.byte_count
        );
        Ok(())
    })();
    let cleanup = support::terminate(&client, session_id, lease);
    support::finish(operation, cleanup)
}
