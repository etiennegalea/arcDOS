package io.arcdos.mobile

import com.connectrpc.Code
import com.connectrpc.ConnectException
import com.connectrpc.ProtocolClientConfig
import com.connectrpc.extensions.GoogleJavaLiteProtobufStrategy
import com.connectrpc.getOrThrow
import com.connectrpc.impl.ProtocolClient
import com.connectrpc.okhttp.ConnectOkHttpClient
import com.google.protobuf.ByteString
import io.arcdos.files.v1.BeginUploadRequest
import io.arcdos.files.v1.CancelUploadRequest
import io.arcdos.files.v1.CommitUploadRequest
import io.arcdos.files.v1.CreateDirectoryRequest
import io.arcdos.files.v1.Entry
import io.arcdos.files.v1.EntryKind
import io.arcdos.files.v1.FileServiceClient
import io.arcdos.files.v1.GetEntryRequest
import io.arcdos.files.v1.GetUploadRequest
import io.arcdos.files.v1.ListChangesRequest
import io.arcdos.files.v1.ListEntriesRequest
import io.arcdos.files.v1.ListEntriesResponse
import io.arcdos.files.v1.MoveEntryRequest
import io.arcdos.files.v1.TransferServiceClient
import io.arcdos.files.v1.TrashEntryRequest
import io.arcdos.mobile.sync.JournalEvent
import io.arcdos.mobile.sync.JournalPage
import io.arcdos.mobile.sync.JournalSource
import java.io.IOException
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.CharBuffer
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

internal class DriveApi(origin: String, private val token: suspend () -> String?) : JournalSource, UploadTransport {
    private val base = origin.toHttpUrl()
    private val protocol = ProtocolClient(
        httpClient = ConnectOkHttpClient(http),
        config = ProtocolClientConfig(
            host = origin,
            serializationStrategy = GoogleJavaLiteProtobufStrategy(),
            ioCoroutineContext = Dispatchers.IO,
        ),
    )
    private val files = FileServiceClient(protocol)
    private val transfers = TransferServiceClient(protocol)

    private suspend fun bearer(): String = "Bearer ${token() ?: throw SignInRequired()}"
    private suspend fun headers() = mapOf("Authorization" to listOf(bearer()))

    private suspend fun contentRequest(entry: Entry): Request.Builder {
        val url = base.newBuilder().addPathSegments("api/files/v1")
            .addPathSegment(entry.entryId).addPathSegment("content").build()
        return Request.Builder().url(url).header("Authorization", bearer())
            .header("Accept-Encoding", "identity")
    }

    suspend fun entry(id: String): Entry = files.getEntry(
        GetEntryRequest.newBuilder().setEntryId(id).build(), headers(),
    ).getOrThrow().entry

    suspend fun list(parentId: String, pageToken: String = ""): ListEntriesResponse = files.listEntries(
        ListEntriesRequest.newBuilder().setParentId(parentId).setPageSize(100)
            .setPageToken(pageToken).build(), headers(),
    ).getOrThrow()

    suspend fun createFolder(parent: Entry, name: String) {
        files.createDirectory(
            CreateDirectoryRequest.newBuilder().setParentId(parent.entryId).setName(name)
                .setExpectedParentRevision(parent.revision).build(), headers(),
        ).getOrThrow()
    }

    suspend fun rename(entry: Entry, parent: Entry, name: String) {
        files.moveEntry(
            MoveEntryRequest.newBuilder().setEntryId(entry.entryId).setExpectedRevision(entry.revision)
                .setDestinationParentId(parent.entryId).setExpectedParentRevision(parent.revision)
                .setName(name).build(), headers(),
        ).getOrThrow()
    }

    suspend fun trash(entry: Entry) {
        files.trashEntry(
            TrashEntryRequest.newBuilder().setEntryId(entry.entryId)
                .setExpectedRevision(entry.revision).build(), headers(),
        ).getOrThrow()
    }

    override suspend fun changes(afterSequence: Long, pageSize: Int): JournalPage {
        val page = files.listChanges(
            ListChangesRequest.newBuilder().setAfterSequence(afterSequence).setPageSize(pageSize)
                .build(), headers(),
        ).getOrThrow()
        return JournalPage(page.changesList.map {
            JournalEvent(it.sequence, it.entryId, it.kindValue)
        }, page.nextSequence)
    }

    override suspend fun beginUpload(parentId: String, name: String, parentRevision: String,
        size: Long, sha256: ByteArray): String = transfers.beginUpload(
        BeginUploadRequest.newBuilder().setParentId(parentId).setName(name)
            .setExpectedParentRevision(parentRevision).setSizeBytes(size)
            .setSha256(ByteString.copyFrom(sha256)).build(), headers(),
    ).getOrThrow().upload.uploadId

    override suspend fun uploadOffset(uploadId: String): Long? = try {
        transfers.getUpload(GetUploadRequest.newBuilder().setUploadId(uploadId).build(), headers())
            .getOrThrow().upload.committedOffset
    } catch (error: ConnectException) {
        if (error.code != Code.NOT_FOUND) throw error
        null
    }

    override suspend fun uploadChunk(uploadId: String, offset: Long, bytes: ByteArray): Long =
        withContext(Dispatchers.IO) {
            val url = base.newBuilder().addPathSegments("api/files/v1/uploads")
                .addPathSegment(uploadId).build()
            val request = Request.Builder().url(url).header("Authorization", bearer())
                .header("Upload-Offset", offset.toString()).patch(bytes.toRequestBody()).build()
            val call = http.newCall(request)
            call.timeout().timeout(30, TimeUnit.SECONDS)
            call.execute().use { response ->
                if (response.code == 401) throw SignInRequired()
                if (response.code != 204 && response.code != 409) throw IOException("Upload chunk rejected")
                response.headers.values("Upload-Offset").singleOrNull()?.toLongOrNull()
                    ?.takeIf { it >= 0 } ?: throw IOException("Missing or invalid upload offset")
            }
        }

    override suspend fun commitUpload(uploadId: String, parentRevision: String) {
        transfers.commitUpload(
            CommitUploadRequest.newBuilder().setUploadId(uploadId)
                .setExpectedParentRevision(parentRevision).build(), headers(),
        ).getOrThrow()
    }

    suspend fun cancelUpload(uploadId: String) {
        try {
            transfers.cancelUpload(CancelUploadRequest.newBuilder().setUploadId(uploadId).build(), headers())
                .getOrThrow()
        } catch (error: ConnectException) {
            if (error.code != Code.NOT_FOUND) throw error
        }
    }

    suspend fun download(entry: Entry, destination: () -> OutputStream) = withContext(Dispatchers.IO) {
        http.newCall(contentRequest(entry).build()).execute().use { response ->
            if (response.code == 401) throw SignInRequired()
            val body = response.body
            if (response.code != 200 || response.header("ETag") != contentTag(entry) ||
                body.contentLength() != entry.sizeBytes || entry.sizeBytes < 0) {
                throw IOException("File is unavailable or has changed")
            }
            destination().use { output ->
                val input = body.byteStream()
                val buffer = ByteArray(64 * 1024)
                var remaining = entry.sizeBytes
                while (remaining > 0) {
                    ensureActive()
                    val count = input.read(buffer, 0, minOf(remaining, buffer.size.toLong()).toInt())
                    if (count < 0) throw IOException("Incomplete download")
                    output.write(buffer, 0, count)
                    remaining -= count
                }
            }
        }
    }

    suspend fun previewText(entry: Entry): TextPreview = withContext(Dispatchers.IO) {
        if (!supportsTextPreview(entry) || entry.sizeBytes < 0) throw TextPreviewUnavailable()
        val length = minOf(entry.sizeBytes, TEXT_PREVIEW_BYTES)
        val range = if (length == 0L) null else "0-${length - 1}"
        val request = contentRequest(entry)
        if (range != null) request.header("Range", "bytes=$range")
        val call = http.newCall(request.build())
        call.timeout().timeout(20, TimeUnit.SECONDS)

        call.execute().use { response ->
            val expectedStatus = if (range == null) 200 else 206
            val expectedRange = range?.let { "bytes $it/${entry.sizeBytes}" }
            val encoding = response.header("Content-Encoding")
            if (response.code != expectedStatus || response.headers.values("ETag") != listOf(contentTag(entry)) ||
                response.headers.values("Content-Range") != listOfNotNull(expectedRange) ||
                response.headers.values("Content-Length").singleOrNull()?.toLongOrNull() != length ||
                response.body.contentLength() != length ||
                (encoding != null && !encoding.equals("identity", ignoreCase = true))
            ) {
                call.cancel()
                if (response.code == 401) throw SignInRequired()
                throw IOException("File preview is unavailable or the file has changed")
            }
            val source = response.body.source()
            val bytes = source.readByteArray(length)
            if (!source.exhausted()) {
                call.cancel()
                throw IOException("Unexpected preview length")
            }
            ensureActive()
            val truncated = length < entry.sizeBytes
            TextPreview(decodeTextPreview(bytes, truncated), truncated)
        }
    }

    private companion object {
        val http = OkHttpClient.Builder()
            .followRedirects(false)
            .followSslRedirects(false)
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(20, TimeUnit.SECONDS)
            .build()
    }
}

internal class SignInRequired : IOException("Sign in to ArcDOS")

internal class TextPreviewUnavailable : IOException("Text preview requires a UTF-8 text file.")

internal data class TextPreview(val text: String, val truncated: Boolean)

private const val TEXT_PREVIEW_BYTES = 32 * 1024L
private val TEXT_EXTENSIONS = setOf(
    "cfg", "css", "csv", "html", "ini", "json", "jsonl", "log", "md", "sql",
    "toml", "tsv", "txt", "xml", "yaml", "yml",
)
private val TEXT_NAMES = setOf("dockerfile", "license", "makefile", "readme")

internal fun supportsTextPreview(entry: Entry): Boolean =
    entry.kind == EntryKind.ENTRY_KIND_FILE && !entry.trashed &&
        (entry.name.lowercase() in TEXT_NAMES || entry.name.substringAfterLast('.', "").lowercase() in TEXT_EXTENSIONS)

private fun contentTag(entry: Entry) = "\"${entry.revision.encodeToByteArray().toHexString()}\""

private fun decodeTextPreview(bytes: ByteArray, truncated: Boolean): String {
    val output = CharBuffer.allocate(bytes.size)
    // With more bytes on the server, a partial UTF-8 sequence at the range boundary is valid.
    val decoded = Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(bytes), output, !truncated)
    if (decoded.isError) throw TextPreviewUnavailable()
    val text = output.array().concatToString(endIndex = output.position())
    if ('\u0000' in text) throw TextPreviewUnavailable()
    return text
}
