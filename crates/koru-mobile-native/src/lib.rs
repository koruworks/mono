// SPDX-License-Identifier: MPL-2.0

//! Android's canonical mobile API edge.
//!
//! Kotlin sends a generated `koru.api.v1` request as bytes and names one
//! catalog operation. This shim does only dispatch plus Protobuf
//! decode/encode; `koru-mobile-host` owns all behavior and persistence.

use jni::objects::{JByteArray, JObject, JString};
use jni::strings::JNIString;
use jni::sys::jbyteArray;
use jni::{Env, EnvUnowned};
use prost::Message;

mod document_observer;
mod update;

fn protobuf<Req, Res>(bytes: &[u8], call: fn(Req) -> Res) -> Result<Vec<u8>, String>
where
    Req: Message + Default,
    Res: Message,
{
    let request = Req::decode(bytes).map_err(|_| "invalid_protobuf".to_owned())?;
    Ok(call(request).encode_to_vec())
}

fn dispatch(operation: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if operation.starts_with("account_") && bytes.len() > 96 * 1024 {
        return Err("account_payload_too_large".into());
    }
    use koru_contract::mobile_proto as pb;
    match operation {
        "status" => protobuf::<pb::HostStatusRequest, pb::HostStatus>(
            bytes,
            koru_mobile_host::protobuf::status,
        ),
        "link_device" => protobuf::<pb::LinkDeviceRequest, pb::LinkDeviceResult>(
            bytes,
            koru_mobile_host::protobuf::link_device,
        ),
        "advertise_build" => protobuf::<pb::BuildPayload, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::advertise_build,
        ),
        "store.inventory" => protobuf::<pb::InventoryRequest, pb::Inventory>(
            bytes,
            koru_mobile_host::protobuf::inventory,
        ),
        "store.document" => protobuf::<pb::DocumentRequest, pb::DocumentProjection>(
            bytes,
            koru_mobile_host::protobuf::document,
        ),
        "store.blob" => {
            protobuf::<pb::BlobRequest, pb::BlobBytes>(bytes, koru_mobile_host::protobuf::blob)
        }
        "store.blob_status" => protobuf::<pb::BlobStatusRequest, pb::BlobTransferStatus>(
            bytes,
            koru_mobile_host::protobuf::blob_status,
        ),
        "store.cloud_sync_status" => protobuf::<pb::CloudSyncStatusRequest, pb::CloudSyncStatus>(
            bytes,
            koru_mobile_host::protobuf::cloud_sync_status,
        ),
        "store.cloud_waiting" => protobuf::<pb::CloudWaitingRequest, pb::CloudWaitingDocuments>(
            bytes,
            koru_mobile_host::protobuf::cloud_waiting,
        ),
        "store.set_blob_policy" => protobuf::<pb::SetBlobPolicyRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::set_blob_policy,
        ),
        "store.request_blob" => protobuf::<pb::RequestBlobRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::request_blob,
        ),
        "store.file_blob" => protobuf::<pb::FileBlobRequest, pb::FileBlob>(
            bytes,
            koru_mobile_host::protobuf::file_blob,
        ),
        "store.export_blob" => protobuf::<pb::ExportBlobRequest, pb::ExportedBlob>(
            bytes,
            koru_mobile_host::protobuf::export_blob,
        ),
        "store.release_blobs" => protobuf::<pb::ReleaseBlobsRequest, pb::ReleasedBlobs>(
            bytes,
            koru_mobile_host::protobuf::release_blobs,
        ),
        "store.recover_blobs" => protobuf::<pb::BlobRecoverRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::recover_blobs,
        ),
        "store.blob_arrivals" => protobuf::<pb::BlobArrivalsRequest, pb::BlobArrivals>(
            bytes,
            koru_mobile_host::protobuf::blob_arrivals,
        ),
        "store.run" => {
            protobuf::<pb::RunRequest, pb::RunView>(bytes, koru_mobile_host::protobuf::run)
        }
        "store.run_timeline" => protobuf::<pb::RunTimelineRequest, pb::RunTimelineWindow>(
            bytes,
            koru_mobile_host::protobuf::run_timeline,
        ),
        "store.mutate_workspace" => protobuf::<pb::WorkspaceMutation, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::mutate_workspace,
        ),
        "store.mutate_profile" => protobuf::<pb::ProfileMutation, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::mutate_profile,
        ),
        "store.mutate_document" => protobuf::<pb::DocumentMutation, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::mutate_document,
        ),
        "store.import_file" => protobuf::<pb::ImportFileRequest, pb::DocumentIdResult>(
            bytes,
            koru_mobile_host::protobuf::import_file,
        ),
        "store.ensure_file_note" => protobuf::<pb::FileNoteRequest, pb::DocumentIdResult>(
            bytes,
            koru_mobile_host::protobuf::file_note,
        ),
        "start_pairing" => protobuf::<pb::StartPairingRequest, pb::PairingOffer>(
            bytes,
            koru_mobile_host::protobuf::start_pairing,
        ),
        "cancel_pairing" => protobuf::<pb::CancelPairingRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::cancel_pairing,
        ),
        "devices" => protobuf::<pb::DevicesRequest, pb::DeviceList>(
            bytes,
            koru_mobile_host::protobuf::devices,
        ),
        "device_history" => protobuf::<pb::DeviceHistoryRequest, pb::DeviceHistory>(
            bytes,
            koru_mobile_host::protobuf::device_history,
        ),
        "unlink_device" => protobuf::<pb::UnlinkDeviceRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::unlink_device,
        ),
        "rename_device" => protobuf::<pb::RenameDeviceRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::rename_device,
        ),
        "reset_identity" => protobuf::<pb::ResetIdentityRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::reset_identity,
        ),
        "set_relay" => protobuf::<pb::SetRelayRequest, pb::MutationResult>(
            bytes,
            koru_mobile_host::protobuf::set_relay,
        ),
        "account_command" => protobuf::<pb::AccountCommandRequest, pb::AccountCommandReply>(
            bytes,
            koru_mobile_host::protobuf::account_command,
        ),
        "account_poll" => {
            let request =
                pb::AccountPollRequest::decode(bytes).map_err(|_| "invalid_protobuf".to_owned())?;
            let mut response = koru_mobile_host::protobuf::account_poll(request);
            let encoded = response.encode_to_vec();
            koru_mobile_host::protobuf::zeroize_account_poll(&mut response);
            Ok(encoded)
        }
        "account_platform" => {
            let request = koru_mobile_host::protobuf::decode_account_platform(bytes)
                .map_err(str::to_owned)?;
            Ok(koru_mobile_host::protobuf::account_platform(request).encode_to_vec())
        }
        _ => Err("operation_not_catalogued".to_owned()),
    }
}

fn read(env: &mut Env, value: JString) -> jni::errors::Result<String> {
    value
        .mutf8_chars(env)
        .map(|value| value.to_str().into_owned())
}

/// Trusted build configuration has its own JNI entry. It is deliberately not
/// a product operation accepted by `nativeInvoke` or an incoming intent.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_koru_mobile_services_KoruApiNative_nativeConfigureAccount<
    'caller,
>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    context: JByteArray<'caller>,
    origin: JString<'caller>,
    issuer: JString<'caller>,
    client: JString<'caller>,
    channel: JString<'caller>,
    rollout: JString<'caller>,
    blob_byte_origins: JString<'caller>,
) {
    unowned_env
        .with_env(|env| {
            let bytes = zeroize::Zeroizing::new(env.convert_byte_array(&context)?);
            let result = (|| -> Result<(), String> {
                if bytes.len() > 16 * 1024 {
                    return Err("account_payload_too_large".into());
                }
                let mut host = koru_contract::mobile_proto::HostContext::decode(bytes.as_slice())
                    .map_err(|_| "invalid_protobuf")?;
                let seeds = zeroize::Zeroizing::new(std::mem::take(&mut host.secrets));
                let secrets = zeroize::Zeroizing::new(
                    <[u8; 64]>::try_from(seeds.as_slice()).map_err(|_| "invalid_device_seeds")?,
                );
                if host.root.is_empty() {
                    return Err("missing_host_context".into());
                }
                koru_mobile_host::configure_account(
                    host.root.into(),
                    *secrets,
                    &host.device_name,
                    host.relay_enabled.unwrap_or(true),
                    koru_mobile_host::account::TrustedAccountConfig {
                        origin: read(env, origin).map_err(|_| "account_configuration_invalid")?,
                        issuer: read(env, issuer).map_err(|_| "account_configuration_invalid")?,
                        google_server_client_id: read(env, client)
                            .map_err(|_| "account_configuration_invalid")?,
                        credential_channel: read(env, channel)
                            .map_err(|_| "account_configuration_invalid")?,
                        platform: koru_account_core::DevicePlatform::Android,
                        credential_rollout: {
                            let value =
                                read(env, rollout).map_err(|_| "account_configuration_invalid")?;
                            if value.trim().is_empty() {
                                None
                            } else {
                                Some(
                                    koru_account_session::RegionalCredentialPolicy::parse(&value)
                                        .map_err(|_| "account_configuration_invalid")?,
                                )
                            }
                        },
                        // Marshalled and nothing else: whether this names a
                        // place a file may be sent is `koru-blob-sync`'s
                        // allowlist to answer, and a shim that pre-judged it
                        // would be a second answer to that question.
                        blob_byte_origins: read(env, blob_byte_origins)
                            .map_err(|_| "account_configuration_invalid")?,
                    },
                )
                .map_err(|_| "account_configuration_unavailable".to_owned())
            })();
            if let Err(code) = result {
                env.throw_new(
                    JNIString::new("java/lang/RuntimeException"),
                    JNIString::new(&code),
                )?;
                return Err(jni::errors::Error::JavaException);
            }
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_koru_mobile_services_KoruApiNative_nativeInvoke<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    operation: JString<'caller>,
    request: JByteArray<'caller>,
) -> jbyteArray {
    unowned_env
        .with_env(|env| {
            let operation = read(env, operation)?;
            let request = zeroize::Zeroizing::new(env.convert_byte_array(&request)?);
            let response = zeroize::Zeroizing::new(match dispatch(&operation, &request) {
                Ok(response) => response,
                Err(code) => {
                    env.throw_new(
                        JNIString::new("java/lang/RuntimeException"),
                        JNIString::new(&code),
                    )?;
                    return Err(jni::errors::Error::JavaException);
                }
            });
            env.byte_array_from_slice(&response)
                .map(|bytes| bytes.into_raw())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_contract::mobile_proto as pb;

    #[test]
    fn unknown_operation_is_rejected() {
        assert_eq!(
            dispatch("not.catalogued", &[]).unwrap_err(),
            "operation_not_catalogued"
        );
    }

    #[test]
    fn malformed_protobuf_is_rejected() {
        assert_eq!(dispatch("status", &[0xff]).unwrap_err(), "invalid_protobuf");
    }

    #[test]
    fn request_types_are_callable_generated_messages() {
        let request = pb::HostStatusRequest::default().encode_to_vec();
        let response = dispatch("status", &request).expect("dispatch");
        pb::HostStatus::decode(response.as_slice()).expect("generated response");
    }
}
