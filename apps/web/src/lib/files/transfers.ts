import { Code, ConnectError } from '@connectrpc/connect';
import { sha256 } from '@noble/hashes/sha2.js';
import type { Entry, Upload } from '../gen/arcdos/files/v1/files_pb';
import type { FileClient } from './client';

const hashChunk = 1024 * 1024;
const uploadChunk = 8 * 1024 * 1024;
const browserDownloadLimit = 64 * 1024 * 1024;
const rpcTimeout = 10_000;
const commitTimeout = 120_000;
const chunkTimeout = 100_000;

export type TransferStatus =
  | 'queued'
  | 'choosing'
  | 'hashing'
  | 'uploading'
  | 'committing'
  | 'downloading'
  | 'canceling'
  | 'completed'
  | 'failed'
  | 'canceled';

export interface TransferJob {
  id: number;
  name: string;
  direction: 'upload' | 'download';
  status: TransferStatus;
  bytes: number;
  total: number;
  message?: string;
  canCancel: boolean;
  canRetry: boolean;
  canDismiss: boolean;
}

interface UploadTask {
  kind: 'upload';
  client: FileClient;
  file: File;
  parentId: string;
  digest?: Uint8Array;
  uploadId?: string;
  readyToCommit?: boolean;
}

interface DownloadTask {
  kind: 'download';
  client: FileClient;
  entryId: string;
  handle?: FileSystemFileHandle;
}

interface Job {
  view: Omit<TransferJob, 'canCancel' | 'canRetry' | 'canDismiss'>;
  task?: UploadTask | DownloadTask;
  controller?: AbortController;
  cancelRequested?: boolean;
}

type Progress = (status: TransferStatus, bytes: number, total?: number) => void;

export function createTransferQueue(
  onChange: (jobs: readonly TransferJob[]) => void,
  onUpload: (entry: Entry) => void
) {
  const jobs = new Map<number, Job>();
  let serial = 0;
  let active: Job | undefined;

  function publish() {
    const history = [...jobs.values()].filter(
      ({ view }) => view.status === 'completed' || view.status === 'canceled'
    );
    for (const job of history.slice(0, Math.max(0, history.length - 20))) jobs.delete(job.view.id);
    onChange([...jobs.values()].map(({ view, task }) => {
      const terminal = ['completed', 'failed', 'canceled'].includes(view.status);
      const hasStage = task?.kind === 'upload' && !!task.uploadId;
      return {
        ...view,
        canCancel: (view.status === 'failed' && hasStage)
          || (!terminal && view.status !== 'canceling' && view.status !== 'committing'),
        canRetry: view.status === 'failed' && !!task,
        canDismiss: terminal && !hasStage
      };
    }));
  }

  function update(job: Job, changes: Partial<Job['view']>) {
    Object.assign(job.view, changes);
    if (jobs.has(job.view.id)) publish();
  }

  function add(task: NonNullable<Job['task']>, name: string, total: number): Job {
    const job: Job = {
      view: { id: ++serial, name, direction: task.kind, status: 'queued', bytes: 0, total },
      task
    };
    jobs.set(job.view.id, job);
    return job;
  }

  function upload(client: FileClient, files: readonly File[], parentId: string) {
    for (const file of files) add({ kind: 'upload', client, file, parentId }, file.name, file.size);
    publish();
    pump();
  }

  function download(client: FileClient, entry: Entry) {
    const job = add({ kind: 'download', client, entryId: entry.entryId }, entry.name, Number(entry.sizeBytes));
    queueDownload(job);
  }

  function queueDownload(job: Job) {
    const task = job.task;
    if (task?.kind !== 'download') return;
    if (job.view.total <= browserDownloadLimit || task.handle) {
      update(job, { status: 'queued', bytes: 0, message: undefined });
      pump();
      return;
    }

    const browser = window as Window & {
      showSaveFilePicker?: (options: { suggestedName: string }) => Promise<FileSystemFileHandle>;
    };
    const picker = browser.showSaveFilePicker?.bind(browser);
    if (!picker) {
      update(job, { status: 'failed', message: 'Downloads over 64 MiB require a browser with a file picker.' });
      return;
    }
    update(job, { status: 'choosing', message: undefined });
    // The picker must be invoked while handling the user's click.
    void chooseFile(task, picker);
    async function chooseFile(download: DownloadTask, pick: NonNullable<typeof picker>) {
      try {
        const handle = await pick({ suggestedName: job.view.name });
        if (!jobs.has(job.view.id) || job.cancelRequested) return;
        download.handle = handle;
        update(job, { status: 'queued', bytes: 0 });
        pump();
      } catch (error) {
        if (job.cancelRequested || !jobs.has(job.view.id)) return;
        if (error instanceof DOMException && error.name === 'AbortError') {
          job.task = undefined;
          update(job, { status: 'canceled' });
        } else {
          update(job, { status: 'failed', message: transferError(error) });
        }
      }
    }
  }

  function pump() {
    if (active) return;
    const job = [...jobs.values()].find(({ view }) => view.status === 'queued');
    if (!job?.task) return;
    active = job;
    const task = job.task;
    const controller = new AbortController();
    job.controller = controller;
    void run(job, task, controller.signal);
  }

  async function run(job: Job, task: UploadTask | DownloadTask, signal: AbortSignal) {
    try {
      const progress: Progress = (status, bytes, total) => {
        if (!job.cancelRequested) update(job, { status, bytes, total: total ?? job.view.total });
      };
      const entry = task.kind === 'upload'
        ? await uploadFile(task, signal, progress)
        : await downloadFile(task, job.view.name, signal, progress);
      if (job.cancelRequested) {
        await finishCancellation(job);
        return;
      }
      job.task = undefined;
      update(job, { status: 'completed', bytes: job.view.total, message: undefined });
      if (entry && jobs.has(job.view.id)) onUpload(entry);
    } catch (error) {
      if (job.cancelRequested) await finishCancellation(job);
      else update(job, { status: 'failed', message: transferError(error) });
    } finally {
      job.controller = undefined;
      active = undefined;
      pump();
    }
  }

  async function finishCancellation(job: Job) {
    const task = job.task;
    try {
      if (task?.kind === 'upload' && task.uploadId) await cancelStage(task, task.uploadId);
      job.task = undefined;
      update(job, { status: 'canceled', message: undefined });
    } catch {
      job.cancelRequested = false;
      update(job, { status: 'failed', message: 'The server could not cancel this upload. Try again.' });
    }
  }

  function cancel(id: number) {
    const job = jobs.get(id);
    if (!job?.task || ['canceling', 'committing', 'completed', 'canceled'].includes(job.view.status)) return;
    job.cancelRequested = true;
    update(job, { status: 'canceling', message: undefined });
    if (job.controller) job.controller.abort();
    else void finishCancellation(job);
  }

  function retry(id: number) {
    const job = jobs.get(id);
    if (!job?.task || job.view.status !== 'failed') return;
    if (job.task.kind === 'download') {
      queueDownload(job);
    } else {
      update(job, { status: 'queued', message: undefined });
      pump();
    }
  }

  function dismiss(id: number) {
    const job = jobs.get(id);
    if (!job || !['completed', 'failed', 'canceled'].includes(job.view.status)) return;
    if (job.task?.kind === 'upload' && job.task.uploadId) return;
    jobs.delete(id);
    publish();
  }

  function dispose() {
    for (const job of jobs.values()) {
      if (job.view.status !== 'committing') cancel(job.view.id);
    }
    jobs.clear();
    publish();
  }

  return { upload, download, cancel, retry, dismiss, dispose };
}

async function uploadFile(task: UploadTask, signal: AbortSignal, progress: Progress): Promise<Entry> {
  const { client, file, parentId } = task;
  let initialOffset: number | undefined;
  if (!task.uploadId) {
    if (!task.digest) {
      progress('hashing', 0, file.size);
      const hash = sha256.create();
      for (let offset = 0; offset < file.size; offset += hashChunk) {
        signal.throwIfAborted();
        const chunk = await file.slice(offset, offset + hashChunk).arrayBuffer();
        hash.update(new Uint8Array(chunk));
        progress('hashing', offset + chunk.byteLength, file.size);
      }
      task.digest = hash.digest();
    }
    const revision = await parentRevision(client, parentId, signal);
    signal.throwIfAborted();
    // Keep the request alive on cancellation long enough to obtain the upload ID.
    const { upload } = await client.transfers.beginUpload({
      parentId,
      name: file.name,
      expectedParentRevision: revision,
      sizeBytes: BigInt(file.size),
      sha256: task.digest
    }, { timeoutMs: rpcTimeout });
    if (!upload?.uploadId) throw new Error('The server did not return an upload ID.');
    task.uploadId = upload.uploadId;
    initialOffset = uploadOffset(upload, task.uploadId, file.size);
    signal.throwIfAborted();
  }

  if (!task.readyToCommit) {
    const id = task.uploadId;
    const getOffset = async () => {
      const { upload } = await client.transfers.getUpload({ uploadId: id }, { signal, timeoutMs: rpcTimeout });
      return uploadOffset(upload, id, file.size);
    };
    let offset = initialOffset ?? await getOffset();
    progress('uploading', offset, file.size);
    let stalledAttempts = 0;
    while (offset < file.size) {
      signal.throwIfAborted();
      const end = Math.min(file.size, offset + uploadChunk);
      const recovered = await sendChunk(client, id, file.slice(offset, end), offset, signal);
      if (recovered === end) {
        offset = end;
        stalledAttempts = 0;
      } else {
        // A timed-out request can still have committed. Reconcile before replaying its bytes.
        const confirmed = await getOffset();
        if (confirmed < offset || confirmed > end) throw new Error('The server returned an unexpected upload offset.');
        stalledAttempts = confirmed === offset ? stalledAttempts + 1 : 0;
        offset = confirmed;
        if (stalledAttempts >= 3) throw new Error('Upload paused. Retry to resume from the server’s saved offset.');
        if (stalledAttempts) await abortableDelay(250 * stalledAttempts, signal);
      }
      progress('uploading', offset, file.size);
    }
    task.readyToCommit = true;
  }

  for (let attempt = 0; ; attempt++) {
    const revision = await parentRevision(client, parentId, signal);
    signal.throwIfAborted();
    progress('committing', file.size, file.size);
    try {
      const { entry } = await client.transfers.commitUpload({
        uploadId: task.uploadId,
        expectedParentRevision: revision
      }, { timeoutMs: commitTimeout });
      if (!entry) throw new Error('The server did not return the published file.');
      return entry;
    } catch (error) {
      if (attempt > 0 || ConnectError.from(error).code !== Code.Aborted) throw error;
    }
  }
}

async function sendChunk(client: FileClient, id: string, bytes: Blob, offset: number, signal: AbortSignal) {
  const requestSignal = AbortSignal.any([signal, AbortSignal.timeout(chunkTimeout)]);
  let response: Response;
  try {
    response = await client.patch(id, bytes, offset, requestSignal);
  } catch (error) {
    signal.throwIfAborted();
    if (error instanceof TypeError || requestSignal.aborted) return;
    throw error;
  }
  if (response.status === 204) {
    const committed = response.headers.get('Upload-Offset');
    return committed === null ? undefined : Number(committed);
  }
  await response.body?.cancel().catch(() => undefined);
  if ([408, 409, 425, 429].includes(response.status) || response.status >= 500) return;
  throw new Error(`Upload chunk failed (HTTP ${response.status}).`);
}

function uploadOffset(upload: Upload | undefined, id: string, size: number) {
  if (!upload || upload.uploadId !== id || upload.sizeBytes !== BigInt(size)
    || upload.committedOffset < 0n || upload.committedOffset > upload.sizeBytes) {
    throw new Error('The server returned an invalid upload status.');
  }
  return Number(upload.committedOffset);
}

async function parentRevision(client: FileClient, parentId: string, signal: AbortSignal) {
  const { entry } = await client.files.getEntry({ entryId: parentId }, { signal, timeoutMs: rpcTimeout });
  if (!entry?.revision || entry.trashed) throw new Error('The destination folder is unavailable.');
  return entry.revision;
}

async function cancelStage(task: UploadTask, uploadId: string) {
  for (let attempt = 0; ; attempt++) {
    try {
      await task.client.transfers.cancelUpload({ uploadId }, { timeoutMs: rpcTimeout });
      return;
    } catch (error) {
      const code = ConnectError.from(error).code;
      if (code === Code.NotFound) return;
      if (attempt >= 2 || code !== Code.FailedPrecondition) throw error;
      await new Promise<void>((resolve) => setTimeout(resolve, 300 * (attempt + 1)));
    }
  }
}

async function abortableDelay(milliseconds: number, signal: AbortSignal) {
  signal.throwIfAborted();
  const timer = AbortSignal.timeout(milliseconds);
  const elapsed = AbortSignal.any([timer, signal]);
  await new Promise<void>((resolve) => elapsed.addEventListener('abort', () => resolve(), { once: true }));
  signal.throwIfAborted();
}

async function downloadFile(task: DownloadTask, name: string, signal: AbortSignal, progress: Progress) {
  progress('downloading', 0);
  const response = await task.client.content(task.entryId, { signal });
  if (!response.ok || !response.body) {
    await response.body?.cancel().catch(() => undefined);
    throw new Error(`Download failed (HTTP ${response.status}).`);
  }
  const length = response.headers.get('Content-Length');
  const total = length === null ? undefined : Number(length);
  const reader = response.body.getReader();
  const parts: ArrayBuffer[] = [];
  let received = 0;
  let output: FileSystemWritableFileStream | undefined;
  try {
    progress('downloading', 0, total);
    if (!task.handle && total !== undefined && total > browserDownloadLimit) {
      throw new Error('The file grew past the browser download limit. Retry to choose a save location.');
    }
    output = await task.handle?.createWritable();
    for (;;) {
      signal.throwIfAborted();
      const { value, done } = await reader.read();
      if (done) break;
      received += value.byteLength;
      if (output) {
        await output.write(value);
      } else {
        if (received > browserDownloadLimit) {
          progress('downloading', received, received);
          throw new Error('The download exceeds 64 MiB. Retry to choose a save location.');
        }
        parts.push(value.slice().buffer);
      }
      progress('downloading', received, total);
    }
    signal.throwIfAborted();
    if (total !== undefined && received !== total) throw new Error('The download ended before all bytes arrived.');
    if (output) await output.close();
    else saveBlob(new Blob(parts, { type: 'application/octet-stream' }), name);
    progress('downloading', received, received);
  } catch (error) {
    await output?.abort().catch(() => undefined);
    throw error;
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
}

function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.hidden = true;
  document.body.append(link);
  try {
    link.click();
  } finally {
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
}

function transferError(error: unknown) {
  if (error instanceof ConnectError && error.rawMessage) return error.rawMessage;
  return error instanceof Error && error.message ? error.message : 'The transfer failed.';
}
