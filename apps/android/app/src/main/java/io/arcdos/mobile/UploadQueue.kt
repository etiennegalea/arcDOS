package io.arcdos.mobile

import android.content.Context
import android.util.AtomicFile
import java.io.File
import java.io.FileNotFoundException
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.RandomAccessFile
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.json.JSONException
import org.json.JSONObject

internal interface UploadTransport {
    suspend fun beginUpload(
        parentId: String,
        name: String,
        parentRevision: String,
        size: Long,
        sha256: ByteArray,
    ): String

    /** Null means GetUpload returned NOT_FOUND. */
    suspend fun uploadOffset(uploadId: String): Long?

    /** Return the server's offset from either a successful chunk or an offset conflict. */
    suspend fun uploadChunk(uploadId: String, offset: Long, bytes: ByteArray): Long

    suspend fun commitUpload(uploadId: String, parentRevision: String)
}

internal data class PendingUpload(
    val parentId: String,
    val name: String,
    val parentRevision: String,
    val size: Long,
    val uploadId: String? = null,
)

internal class UploadQueue(context: Context) {
    private val directory = context.applicationContext.noBackupFilesDir
    private val metadata = AtomicFile(File(directory, "arcdos-pending-upload.json"))
    private val payload = File(directory, "arcdos-pending-upload.bin")
    private val temporary = File(directory, "arcdos-pending-upload.tmp")
    private val lock = Mutex()

    private data class Record(val scope: String, val upload: PendingUpload, val digest: ByteArray)

    suspend fun pending(scope: String): PendingUpload? = lock.withLock {
        withContext(Dispatchers.IO) { read()?.takeIf { it.scope == scope }?.upload }
    }

    suspend fun stage(
        scope: String,
        parentId: String,
        name: String,
        parentRevision: String,
        openSource: () -> InputStream,
    ): PendingUpload = lock.withLock {
        withContext(Dispatchers.IO) {
            validateTarget(scope, parentId, name, parentRevision)
            if (read() != null) throw IOException("Finish or discard the pending upload first.")
            Files.deleteIfExists(payload.toPath())
            Files.deleteIfExists(temporary.toPath())
            val digest = MessageDigest.getInstance("SHA-256")
            var size = 0L
            try {
                openSource().use { input ->
                    FileOutputStream(temporary).use { output ->
                        val buffer = ByteArray(64 * 1024)
                        while (true) {
                            ensureActive()
                            val count = input.read(buffer)
                            if (count < 0) break
                            if (count == 0) throw IOException("Could not read the selected file.")
                            size += count
                            if (size > MAX_BYTES) throw IOException("Uploads are limited to 64 MiB.")
                            output.write(buffer, 0, count)
                            digest.update(buffer, 0, count)
                        }
                        output.fd.sync()
                    }
                }
                ensureActive()
                Files.move(temporary.toPath(), payload.toPath(), StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING)
                val upload = PendingUpload(parentId, name, parentRevision, size)
                save(Record(scope, upload, digest.digest()))
                upload
            } finally {
                temporary.delete()
            }
        }
    }

    suspend fun resume(scope: String, transport: UploadTransport) {
        lock.withLock {
            withContext(Dispatchers.IO) {
                val record = read()?.takeIf { it.scope == scope }
                    ?: throw IOException("There is no pending upload for this sign-in.")
                val upload = record.upload
                if (!payload.isFile || payload.length() != upload.size) {
                    throw IOException("The staged file is missing or incomplete.")
                }
                val id = upload.uploadId ?: transport.beginUpload(
                    upload.parentId, upload.name, upload.parentRevision, upload.size, record.digest,
                ).also { newId ->
                    if (!validId(newId)) throw IOException("Invalid upload ID returned by the server.")
                    withContext(NonCancellable) {
                        save(record.copy(upload = upload.copy(uploadId = newId)))
                    }
                }
                val offset = transport.uploadOffset(id)
                if (offset != null) appendRemaining(transport, id, upload.size, offset)
                // An absent staging record may mean a prior commit succeeded before its reply arrived.
                ensureActive()
                transport.commitUpload(id, upload.parentRevision)
                withContext(NonCancellable) { clearLocal() }
            }
        }
    }

    suspend fun discard() {
        lock.withLock { withContext(Dispatchers.IO) { clearLocal() } }
    }

    suspend fun discard(scope: String): Boolean = lock.withLock {
        withContext(Dispatchers.IO) {
            if (read()?.scope != scope) return@withContext false
            clearLocal()
            true
        }
    }

    private suspend fun appendRemaining(transport: UploadTransport, id: String, size: Long, initialOffset: Long) {
        var offset = initialOffset
        if (offset !in 0..size) throw IOException("Invalid upload offset returned by the server.")
        RandomAccessFile(payload, "r").use { input ->
            var requests = 0
            var stalled = 0
            var furthestOffset = offset
            while (offset < size) {
                currentCoroutineContext().ensureActive()
                if (++requests > MAX_CHUNK_REQUESTS) throw IOException("Upload stopped making progress.")
                input.seek(offset)
                val bytes = ByteArray(minOf(CHUNK_BYTES.toLong(), size - offset).toInt())
                input.readFully(bytes)
                val next = transport.uploadChunk(id, offset, bytes)
                if (next !in 0..size) throw IOException("Invalid upload offset returned by the server.")
                if (next > furthestOffset) {
                    furthestOffset = next
                    stalled = 0
                } else if (++stalled == 3) {
                    throw IOException("Upload stopped making progress.")
                }
                offset = next
            }
        }
    }

    private fun save(record: Record) {
        val upload = record.upload
        val json = JSONObject()
            .put("version", 1)
            .put("scope", record.scope)
            .put("parent", upload.parentId)
            .put("name", upload.name)
            .put("revision", upload.parentRevision)
            .put("size", upload.size)
            .put("id", upload.uploadId ?: JSONObject.NULL)
            .put("sha256", record.digest.toHexString())
        val stream = metadata.startWrite()
        try {
            stream.write(json.toString().encodeToByteArray())
            stream.fd.sync()
            metadata.finishWrite(stream)
        } catch (error: IOException) {
            metadata.failWrite(stream)
            throw error
        }
        val saved = read()
        if (saved == null || saved.upload != upload || saved.scope != record.scope ||
            !saved.digest.contentEquals(record.digest)) {
            throw IOException("Could not save the pending upload.")
        }
    }

    private fun read(): Record? {
        val source = try {
            metadata.openRead()
        } catch (error: FileNotFoundException) {
            if (metadata.baseFile.exists()) throw error
            return null
        }
        return try {
            val json = source.use {
                if (it.channel.size() !in 1..MAX_METADATA_BYTES) throw IOException("Invalid upload state size.")
                JSONObject(it.readBytes().decodeToString())
            }
            if (json.getInt("version") != 1) throw IOException("Unsupported upload state.")
            val scope = json.getString("scope")
            val upload = PendingUpload(
                parentId = json.getString("parent"),
                name = json.getString("name"),
                parentRevision = json.getString("revision"),
                size = json.getLong("size"),
                uploadId = if (json.isNull("id")) null else json.getString("id"),
            )
            validateTarget(scope, upload.parentId, upload.name, upload.parentRevision)
            val digest = json.getString("sha256").hexToByteArray()
            if (upload.size !in 0..MAX_BYTES || digest.size != 32 ||
                (upload.uploadId != null && !validId(upload.uploadId))) {
                throw IOException("Invalid pending upload state.")
            }
            Record(scope, upload, digest)
        } catch (error: JSONException) {
            throw IOException("Invalid pending upload state.", error)
        } catch (error: IllegalArgumentException) {
            throw IOException("Invalid pending upload state.", error)
        }
    }

    private fun clearLocal() {
        metadata.delete()
        if (metadata.baseFile.exists()) throw IOException("Could not clear the pending upload state.")
        Files.deleteIfExists(payload.toPath())
        Files.deleteIfExists(temporary.toPath())
    }

    private companion object {
        const val MAX_BYTES = 64L * 1024 * 1024
        const val CHUNK_BYTES = 512 * 1024
        const val MAX_CHUNK_REQUESTS = (MAX_BYTES / CHUNK_BYTES).toInt() + 8
        const val MAX_METADATA_BYTES = 8192L

        fun validId(id: String): Boolean = id.length in 1..128 && id.all { it in '!'..'~' }

        fun validateTarget(scope: String, parentId: String, name: String, revision: String) {
            require(validId(scope) && (parentId.isEmpty() || validId(parentId)) && validId(revision)) {
                "Invalid upload destination."
            }
            require(name.encodeToByteArray().size in 1..255 && name != "." && name != ".." &&
                name.none { it == '/' || it == '\\' || it == '\u0000' }) { "Invalid file name." }
        }
    }
}
