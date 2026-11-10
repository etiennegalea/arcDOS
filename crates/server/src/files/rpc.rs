use std::sync::Arc;

use arcdos_filesystem::Kind;
use arcdos_protocol::{connect::arcdos::files::v1 as service, proto::arcdos::files::v1 as pb};
use arcdos_protocol::{
    connect::arcdos::search::v1 as search_service, proto::arcdos::search::v1 as search_pb,
};
use arcdos_transfers::{TransferError, Upload, UploadSpec};
use buffa_types::google::protobuf::Timestamp;
use connectrpc::{ConnectError, Limits, RequestContext, Response, ServiceRequest, ServiceResult};
use uuid::Uuid;

use super::{Error, FileApi, Result, catalog::Change, store::Resource};

pub(super) fn router(api: Arc<FileApi>) -> axum::Router {
    let procedures = [
        service::FILE_SERVICE_GET_ENTRY_SPEC.procedure,
        service::FILE_SERVICE_LIST_ENTRIES_SPEC.procedure,
        service::FILE_SERVICE_CREATE_DIRECTORY_SPEC.procedure,
        service::FILE_SERVICE_MOVE_ENTRY_SPEC.procedure,
        service::FILE_SERVICE_COPY_ENTRY_SPEC.procedure,
        service::FILE_SERVICE_TRASH_ENTRY_SPEC.procedure,
        service::FILE_SERVICE_RESTORE_ENTRY_SPEC.procedure,
        service::FILE_SERVICE_LIST_TRASH_SPEC.procedure,
        service::FILE_SERVICE_LIST_CHANGES_SPEC.procedure,
        service::TRANSFER_SERVICE_BEGIN_UPLOAD_SPEC.procedure,
        service::TRANSFER_SERVICE_GET_UPLOAD_SPEC.procedure,
        service::TRANSFER_SERVICE_COMMIT_UPLOAD_SPEC.procedure,
        service::TRANSFER_SERVICE_CANCEL_UPLOAD_SPEC.procedure,
        search_service::SEARCH_SERVICE_SEARCH_FILES_SPEC.procedure,
        search_service::SEARCH_SERVICE_SET_TAGS_SPEC.procedure,
    ];
    let connect = service::FileServiceExt::register(api.clone(), connectrpc::Router::new());
    let connect = service::TransferServiceExt::register(api.clone(), connect);
    let connect = search_service::SearchServiceExt::register(api, connect);
    let limits = Limits::default()
        .with_max_request_body_size(8192)
        .with_max_message_size(2 * 1024 * 1024)
        .with_element_memory_limit(4 * 1024 * 1024);
    let connect = procedures
        .iter()
        .fold(connect, |router, procedure| {
            router.with_route_limits(procedure, limits)
        })
        .into_axum_service();
    procedures
        .iter()
        .fold(axum::Router::new(), |router, procedure| {
            router.route_service(procedure, connect.clone())
        })
}

#[allow(refining_impl_trait)]
impl search_service::SearchService for FileApi {
    async fn search_files(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, search_pb::SearchFilesRequest>,
    ) -> ServiceResult<search_pb::SearchFilesResponse> {
        let request = request.to_owned_message();
        let (hits, has_more) = self
            .run(move |store| store.search(&request.query, request.limit))
            .await?;
        Response::ok(search_pb::SearchFilesResponse {
            hits: hits
                .into_iter()
                .map(|hit| search_pb::SearchHit {
                    entry: to_entry(hit.entry).into(),
                    path: hit.path,
                    tags: hit.tags,
                    ..Default::default()
                })
                .collect(),
            has_more,
            ..Default::default()
        })
    }

    async fn set_tags(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, search_pb::SetTagsRequest>,
    ) -> ServiceResult<search_pb::SetTagsResponse> {
        let request = request.to_owned_message();
        let tags = self
            .run(move |store| {
                store.set_tags(&request.entry_id, &request.expected_revision, request.tags)
            })
            .await?;
        Response::ok(search_pb::SetTagsResponse {
            tags,
            ..Default::default()
        })
    }
}

#[allow(refining_impl_trait)]
impl service::FileService for FileApi {
    async fn get_entry(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetEntryRequest>,
    ) -> ServiceResult<pb::GetEntryResponse> {
        let id = request.entry_id.to_owned();
        let entry = self.run(move |store| store.get(&id)).await?;
        Response::ok(pb::GetEntryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn list_entries(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListEntriesRequest>,
    ) -> ServiceResult<pb::ListEntriesResponse> {
        let request = request.to_owned_message();
        let page = self
            .run(move |store| {
                store.list(&request.parent_id, request.page_size, &request.page_token)
            })
            .await?;
        Response::ok(pb::ListEntriesResponse {
            entries: page.entries.into_iter().map(to_entry).collect(),
            next_page_token: page.next,
            ..Default::default()
        })
    }

    async fn create_directory(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CreateDirectoryRequest>,
    ) -> ServiceResult<pb::CreateDirectoryResponse> {
        let request = request.to_owned_message();
        let entry = self
            .run(move |store| {
                store.create_directory(
                    &request.parent_id,
                    &request.name,
                    &request.expected_parent_revision,
                )
            })
            .await?;
        Response::ok(pb::CreateDirectoryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn move_entry(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::MoveEntryRequest>,
    ) -> ServiceResult<pb::MoveEntryResponse> {
        let request = request.to_owned_message();
        let entry = self
            .run(move |store| {
                store.move_entry(
                    &request.entry_id,
                    &request.expected_revision,
                    &request.destination_parent_id,
                    &request.name,
                    &request.expected_parent_revision,
                )
            })
            .await?;
        Response::ok(pb::MoveEntryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn copy_entry(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CopyEntryRequest>,
    ) -> ServiceResult<pb::CopyEntryResponse> {
        let request = request.to_owned_message();
        let job = self
            .run(move |store| {
                store.plan_copy(
                    &request.entry_id,
                    &request.expected_revision,
                    &request.destination_parent_id,
                    &request.name,
                    &request.expected_parent_revision,
                )
            })
            .await?;
        let job = tokio::task::spawn_blocking(move || job.copy())
            .await
            .map_err(|_| Error::Inconsistent)??;
        let entry = self.run(move |store| store.finish_copy(job)).await?;
        Response::ok(pb::CopyEntryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn trash_entry(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::TrashEntryRequest>,
    ) -> ServiceResult<pb::TrashEntryResponse> {
        let request = request.to_owned_message();
        let entry = self
            .run(move |store| store.trash(&request.entry_id, &request.expected_revision))
            .await?;
        Response::ok(pb::TrashEntryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn restore_entry(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::RestoreEntryRequest>,
    ) -> ServiceResult<pb::RestoreEntryResponse> {
        let request = request.to_owned_message();
        let entry = self
            .run(move |store| store.restore(&request.entry_id, &request.expected_revision))
            .await?;
        Response::ok(pb::RestoreEntryResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn list_trash(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListTrashRequest>,
    ) -> ServiceResult<pb::ListTrashResponse> {
        let request = request.to_owned_message();
        let page = self
            .run(move |store| store.list_trash(request.page_size, &request.page_token))
            .await?;
        Response::ok(pb::ListTrashResponse {
            entries: page.entries.into_iter().map(to_entry).collect(),
            next_page_token: page.next,
            ..Default::default()
        })
    }

    async fn list_changes(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::ListChangesRequest>,
    ) -> ServiceResult<pb::ListChangesResponse> {
        let (after, size) = (request.after_sequence, request.page_size);
        let changes = self.run(move |store| store.changes(after, size)).await?;
        let next_sequence = changes.last().map_or(after, |change| change.sequence);
        Response::ok(pb::ListChangesResponse {
            changes: changes.into_iter().map(to_change).collect(),
            next_sequence,
            ..Default::default()
        })
    }
}

#[allow(refining_impl_trait)]
impl service::TransferService for FileApi {
    async fn begin_upload(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::BeginUploadRequest>,
    ) -> ServiceResult<pb::BeginUploadResponse> {
        let digest = request
            .sha256
            .try_into()
            .map_err(|_| Error::Invalid("SHA-256 must be 32 bytes"))?;
        let length = request.size_bytes;
        let request = request.to_owned_message();
        let target = self
            .run(move |store| {
                store.upload_target(
                    &request.parent_id,
                    &request.name,
                    &request.expected_parent_revision,
                )
            })
            .await?;
        let upload = self
            .uploads
            .create(UploadSpec {
                target,
                length,
                sha256: digest,
            })
            .await
            .map_err(Error::from)?;
        Response::ok(pb::BeginUploadResponse {
            upload: to_upload(upload).into(),
            ..Default::default()
        })
    }

    async fn get_upload(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::GetUploadRequest>,
    ) -> ServiceResult<pb::GetUploadResponse> {
        let id = upload_id(request.upload_id)?;
        let upload = self.uploads.status(id).await.map_err(Error::from)?;
        Response::ok(pb::GetUploadResponse {
            upload: to_upload(upload).into(),
            ..Default::default()
        })
    }

    async fn commit_upload(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CommitUploadRequest>,
    ) -> ServiceResult<pb::CommitUploadResponse> {
        let id = upload_id(request.upload_id)?;
        let expected = request.expected_parent_revision.to_owned();
        let entry = self.commit(id, expected).await?;
        Response::ok(pb::CommitUploadResponse {
            entry: to_entry(entry).into(),
            ..Default::default()
        })
    }

    async fn cancel_upload(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, pb::CancelUploadRequest>,
    ) -> ServiceResult<pb::CancelUploadResponse> {
        let id = upload_id(request.upload_id)?;
        self.uploads.cancel(id).await.map_err(Error::from)?;
        Response::ok(pb::CancelUploadResponse::default())
    }
}

impl FileApi {
    async fn commit(&self, id: Uuid, expected: String) -> Result<Resource> {
        let upload_id = id.to_string();
        let prepared = match self.uploads.prepare_commit(id).await {
            Ok(prepared) => prepared,
            Err(TransferError::NotFound) => {
                return self
                    .run(move |store| store.committed_upload(&upload_id)?.ok_or(Error::NotFound))
                    .await;
            }
            Err(error) => return Err(error.into()),
        };
        let (entry, prepared) = self
            .run(move |store| {
                let entry = if prepared.is_staged() {
                    let target = &prepared.upload().target;
                    let expected = if expected.is_empty() {
                        &target.parent_revision
                    } else {
                        &expected
                    };
                    store.publish_upload(
                        &upload_id,
                        target,
                        expected,
                        prepared.staging_name(),
                        &prepared.identity(),
                    )?
                } else {
                    store
                        .committed_upload(&upload_id)?
                        .ok_or(Error::Inconsistent)?
                };
                Ok((entry, prepared))
            })
            .await?;
        prepared.finish().await?;
        Ok(entry)
    }
}

fn upload_id(id: &str) -> Result<Uuid> {
    Uuid::try_parse(id).map_err(|_| Error::Invalid("invalid upload ID"))
}

fn to_entry(resource: Resource) -> pb::Entry {
    let kind = match resource.entry.kind {
        Kind::File => pb::EntryKind::File,
        Kind::Directory => pb::EntryKind::Directory,
    };
    let trashed = resource.trashed();
    pb::Entry {
        entry_id: resource.id,
        parent_id: resource.parent_id,
        name: resource.name,
        kind: kind.into(),
        size_bytes: if kind == pb::EntryKind::File {
            resource.entry.size
        } else {
            0
        },
        revision: resource.revision,
        modified_at: timestamp(resource.entry.mtime_sec, resource.entry.mtime_nsec as i32).into(),
        trashed,
        ..Default::default()
    }
}

fn to_change(change: Change) -> pb::Change {
    let kind = match change.kind {
        1 => pb::ChangeKind::Created,
        2 => pb::ChangeKind::Moved,
        3 => pb::ChangeKind::Copied,
        4 => pb::ChangeKind::Trashed,
        5 => pb::ChangeKind::Restored,
        6 => pb::ChangeKind::Uploaded,
        7 => pb::ChangeKind::Modified,
        8 => pb::ChangeKind::Removed,
        _ => pb::ChangeKind::Unspecified,
    };
    pb::Change {
        sequence: change.sequence,
        entry_id: change.entry_id,
        kind: kind.into(),
        occurred_at: timestamp(change.seconds, change.nanos).into(),
        ..Default::default()
    }
}

fn to_upload(upload: Upload) -> pb::Upload {
    pb::Upload {
        upload_id: upload.id.to_string(),
        size_bytes: upload.length,
        committed_offset: upload.offset,
        ..Default::default()
    }
}

fn timestamp(seconds: i64, nanos: i32) -> Timestamp {
    Timestamp {
        seconds,
        nanos,
        ..Default::default()
    }
}

impl From<Error> for ConnectError {
    fn from(error: Error) -> Self {
        let message = error.message();
        match error {
            Error::Invalid(_) => Self::invalid_argument(message),
            Error::NotFound => Self::not_found(message),
            Error::Exists => Self::already_exists(message),
            Error::Stale => Self::aborted(message),
            Error::Conflict(_) => Self::failed_precondition(message),
            Error::Forbidden => Self::permission_denied(message),
            Error::Exhausted => Self::resource_exhausted(message),
            _ => Self::internal(message).with_source(error),
        }
    }
}
