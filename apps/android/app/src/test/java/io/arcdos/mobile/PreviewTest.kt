package io.arcdos.mobile

import com.sun.net.httpserver.HttpServer
import io.arcdos.files.v1.Entry
import io.arcdos.files.v1.EntryKind
import java.io.IOException
import java.net.InetSocketAddress
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PreviewTest {
    @Test
    fun validRangeAndEmptyFile() {
        val text = "Ġurnata 🦀"
        val bytes = text.encodeToByteArray()
        serve(bytes, bytes.size.toLong()) { api ->
            assertEquals(TextPreview(text, false), runBlocking { api.previewText(entry(bytes.size.toLong())) })
        }
        serve(byteArrayOf(), 0) { api ->
            assertEquals(TextPreview("", false), runBlocking { api.previewText(entry(0)) })
        }
    }

    @Test
    fun prefixDropsOnlyIncompleteTrailingRune() {
        val prefix = "x".repeat(32_767)
        val full = (prefix + "🦀tail").encodeToByteArray()
        serve(full.copyOf(32_768), full.size.toLong()) { api ->
            assertEquals(TextPreview(prefix, true), runBlocking { api.previewText(entry(full.size.toLong())) })
        }
    }

    @Test
    fun rejectsMalformedUtf8AndBinaryNul() {
        for (invalid in listOf(byteArrayOf(0xC3.toByte()), byteArrayOf(0))) {
            serve(invalid, invalid.size.toLong()) { api ->
                assertThrows(TextPreviewUnavailable::class.java) {
                    runBlocking { api.previewText(entry(invalid.size.toLong())) }
                }
            }
        }
    }

    @Test
    fun rejectsChangedMetadataAndMissingAuthorization() {
        val bytes = "abc".encodeToByteArray()
        serve(bytes, 3, tag = "\"wrong\"") { api ->
            assertThrows(IOException::class.java) { runBlocking { api.previewText(entry(3)) } }
        }
        serve(bytes, 3, range = "bytes 0-1/3") { api ->
            assertThrows(IOException::class.java) { runBlocking { api.previewText(entry(3)) } }
        }
        serve(bytes, 4, range = "bytes 0-3/4") { api ->
            assertThrows(IOException::class.java) { runBlocking { api.previewText(entry(4)) } }
        }
        serve(bytes, 3, status = 401) { api ->
            assertThrows(SignInRequired::class.java) { runBlocking { api.previewText(entry(3)) } }
        }
    }

    private fun entry(size: Long) = Entry.newBuilder()
        .setEntryId("entry")
        .setName("README.md")
        .setKind(EntryKind.ENTRY_KIND_FILE)
        .setSizeBytes(size)
        .setRevision("fixture")
        .build()

    private fun serve(
        bytes: ByteArray,
        length: Long,
        status: Int = if (bytes.isEmpty()) 200 else 206,
        tag: String = "\"66697874757265\"",
        range: String? = if (bytes.isEmpty()) null else "bytes 0-${bytes.lastIndex}/$length",
        check: (DriveApi) -> Unit,
    ) {
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val request = AtomicReference<Triple<String, String?, String?>>()
        server.createContext("/api/files/v1/entry/content") { exchange ->
            request.set(Triple(
                exchange.requestURI.path,
                exchange.requestHeaders.getFirst("Authorization"),
                exchange.requestHeaders.getFirst("Range"),
            ))
            exchange.responseHeaders.set("ETag", tag)
            if (range != null) exchange.responseHeaders.set("Content-Range", range)
            if (bytes.isEmpty()) exchange.responseHeaders.set("Content-Length", "0")
            exchange.sendResponseHeaders(status, if (bytes.isEmpty()) -1 else bytes.size.toLong())
            exchange.responseBody.use { it.write(bytes) }
        }
        server.start()
        try {
            check(DriveApi("http://127.0.0.1:${server.address.port}") { "fixture-token" })
            assertEquals(
                Triple(
                    "/api/files/v1/entry/content",
                    "Bearer fixture-token",
                    if (length == 0L) null else "bytes=0-${minOf(length, 32_768) - 1}",
                ),
                request.get(),
            )
        } finally {
            server.stop(0)
        }
    }
}
