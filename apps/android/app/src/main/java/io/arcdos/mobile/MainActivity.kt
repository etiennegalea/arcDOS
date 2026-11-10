package io.arcdos.mobile

import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.util.Base64
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import com.connectrpc.Code
import com.connectrpc.ConnectException
import io.arcdos.files.v1.Entry
import io.arcdos.files.v1.EntryKind
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : FragmentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val app = application as ArcApplication
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    if (app.configured) {
                        Workspace(app)
                    } else {
                        Text("Configure the ArcDOS HTTPS origin and Authentik client at build time.",
                            Modifier.safeDrawingPadding().padding(16.dp))
                    }
                }
            }
        }
    }

    private fun promptUnlock(onSuccess: () -> Unit, onError: () -> Unit) {
        val prompt = BiometricPrompt(this, ContextCompat.getMainExecutor(this),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    onSuccess()
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    onError()
                }
            })
        prompt.authenticate(BiometricPrompt.PromptInfo.Builder()
            .setTitle("Unlock ArcDOS")
            .setAllowedAuthenticators(BIOMETRIC_WEAK or DEVICE_CREDENTIAL)
            .build())
    }

    @Composable
    private fun Workspace(app: ArcApplication) {
        val session = app.session
        val scope = rememberCoroutineScope()
        var ready by remember { mutableStateOf(session.signedIn && !session.locked) }
        var locked by remember { mutableStateOf(session.locked) }
        var busy by remember { mutableStateOf(false) }
        var message by remember { mutableStateOf<String?>(null) }
        var folderId by rememberSaveable { mutableStateOf("") }
        var folder by remember { mutableStateOf<Entry?>(null) }
        var entries by remember { mutableStateOf<List<Entry>>(emptyList()) }
        var next by remember { mutableStateOf("") }
        var selected by remember { mutableStateOf<Entry?>(null) }
        var preview by remember { mutableStateOf<Pair<String, TextPreview>?>(null) }
        var rename by remember { mutableStateOf<Entry?>(null) }
        var creating by remember { mutableStateOf(false) }
        var confirmingTrash by remember { mutableStateOf(false) }
        var pendingDownload by rememberSaveable { mutableStateOf<String?>(null) }
        var uploadDestination by rememberSaveable { mutableStateOf<String?>(null) }
        var pendingUpload by remember { mutableStateOf<PendingUpload?>(null) }
        var confirmingDiscard by remember { mutableStateOf(false) }

        fun work(action: suspend () -> Unit) {
            if (busy) return
            busy = true
            message = null
            scope.launch {
                try {
                    action()
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    message = when {
                        error is SignInRequired || error is ConnectException &&
                            error.code == Code.UNAUTHENTICATED -> "Session expired. Sign out and sign in again."
                        error is ConnectException && error.code == Code.ABORTED -> "The folder changed. Refresh and retry."
                        error is ConnectException && error.code == Code.ALREADY_EXISTS -> "The destination already exists."
                        error is UploadDestinationChanged -> "The upload destination changed. Discard it and choose the file again."
                        error is ConnectException && error.code == Code.PERMISSION_DENIED -> "ArcDOS access denied."
                        error is TextPreviewUnavailable -> "Text preview requires a UTF-8 text file."
                        else -> "ArcDOS could not complete the request."
                    }
                } finally {
                    ready = session.signedIn && !session.locked
                    if (!ready) preview = null
                    locked = session.locked
                    busy = false
                }
            }
        }

        suspend fun load(id: String) {
            val current = app.drive.entry(id)
            val page = app.drive.list(current.entryId)
            folderId = if (current.parentId.isEmpty()) "" else current.entryId
            folder = current
            entries = page.entriesList
            next = page.nextPageToken
        }

        suspend fun resumeUpload(cache: String) {
            try {
                app.uploads.resume(cache, app.scopedDrive(cache))
            } catch (error: ConnectException) {
                if (error.code == Code.ABORTED) throw UploadDestinationChanged()
                throw error
            } finally {
                pendingUpload = app.uploads.pending(cache)
            }
            message = "Upload complete."
            load(folderId)
        }

        val signIn = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            result.data?.let { data ->
                work {
                    session.completeAuthorization(data)
                    app.journal.clearAll()
                    app.uploads.discard()
                    folderId = ""
                }
            }
        }
        val open = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
            val destination = uploadDestination
            uploadDestination = null
            if (uri != null && uri.scheme == "content" && destination != null) {
                work {
                    val cache = session.cacheScope ?: throw SignInRequired()
                    val parent = Entry.parseFrom(Base64.decode(destination, Base64.NO_WRAP))
                    val name = withContext(Dispatchers.IO) {
                        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                            ?.use { if (it.moveToFirst()) it.getString(0) else null }
                    } ?: throw IOException("Missing document name")
                    pendingUpload = app.uploads.stage(cache, parent.entryId, name, parent.revision) {
                        contentResolver.openInputStream(uri) ?: throw IOException("Cannot read document")
                    }
                    resumeUpload(cache)
                }
            }
        }
        val save = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
            val pending = pendingDownload
            pendingDownload = null
            if (uri != null && pending != null) {
                work {
                    try {
                        val entry = Entry.parseFrom(Base64.decode(pending, Base64.NO_WRAP))
                        app.drive.download(entry) {
                            contentResolver.openOutputStream(uri, "w") ?: throw IOException("Cannot open destination")
                        }
                        message = "Download saved."
                    } catch (error: Exception) {
                        removeIncomplete(uri)
                        throw error
                    }
                }
            }
        }

        LaunchedEffect(ready) {
            if (ready) work {
                pendingUpload = session.cacheScope?.let { app.uploads.pending(it) }
                load(folderId)
            }
        }
        BackHandler(ready && folderId.isNotEmpty() && !busy) {
            val parent = folder?.parentId.orEmpty()
            work { load(parent) }
        }

        Column(Modifier.fillMaxSize().safeDrawingPadding().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("ArcDOS", style = MaterialTheme.typography.headlineMedium)
            if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
            message?.let { Text(it) }
            when {
                locked -> {
                    Button(enabled = !busy, onClick = {
                        promptUnlock(onSuccess = { work { session.unlock() } },
                            onError = { message = "Unlock was cancelled or is unavailable." })
                    }) { Text("Unlock") }
                    TextButton(enabled = !busy, onClick = {
                        work {
                            session.logout()
                            app.journal.clearAll()
                            app.uploads.discard()
                            folderId = ""
                        }
                    }) { Text("Sign out") }
                }
                !ready -> Button(enabled = !busy,
                    onClick = { work { signIn.launch(session.authorizationIntent()) } }) { Text("Sign in") }
                else -> {
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        TextButton(enabled = !busy && folderId.isNotEmpty(),
                            onClick = { work { load(folder?.parentId.orEmpty()) } }) { Text("Up") }
                        TextButton(enabled = !busy, onClick = { work { load(folderId) } }) { Text("Refresh") }
                        TextButton(enabled = !busy, onClick = {
                            work {
                                val cache = session.cacheScope ?: throw SignInRequired()
                                val count = app.journal.sync(cache, app.scopedDrive(cache))
                                message = "$count changes synchronized."
                            }
                        }) { Text("Sync changes") }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        TextButton(enabled = !busy && folder != null,
                            onClick = { creating = true }) { Text("New folder") }
                        TextButton(enabled = !busy, onClick = {
                            work {
                                session.lock()
                                entries = emptyList()
                                folder = null
                                pendingUpload = null
                            }
                        }) { Text("Lock") }
                        TextButton(enabled = !busy, onClick = {
                            work {
                                session.logout()
                                entries = emptyList()
                                folder = null
                                folderId = ""
                                app.journal.clearAll()
                                app.uploads.discard()
                                pendingUpload = null
                            }
                        }) { Text("Sign out") }
                    }
                    val upload = pendingUpload
                    if (upload == null) {
                        TextButton(enabled = !busy && folder != null, onClick = {
                            uploadDestination = Base64.encodeToString(checkNotNull(folder).toByteArray(), Base64.NO_WRAP)
                            open.launch(arrayOf("*/*"))
                        }) { Text("Upload file") }
                    } else {
                        Text("Pending upload · ${upload.name}", maxLines = 1)
                        Row {
                            TextButton(enabled = !busy, onClick = {
                                work { resumeUpload(session.cacheScope ?: throw SignInRequired()) }
                            }) { Text("Resume") }
                            TextButton(enabled = !busy, onClick = { confirmingDiscard = true }) { Text("Discard") }
                        }
                    }
                    Text(folder?.name?.ifEmpty { "Shared files" } ?: "Shared files",
                        style = MaterialTheme.typography.titleLarge)
                    LazyColumn(Modifier.weight(1f)) {
                        items(entries, key = Entry::getEntryId) { entry ->
                            Row(Modifier.fillMaxWidth().clickable(enabled = !busy) {
                                if (entry.kind == EntryKind.ENTRY_KIND_DIRECTORY) work { load(entry.entryId) }
                                else selected = entry
                            }, verticalAlignment = Alignment.CenterVertically) {
                                Text(entry.name, Modifier.weight(1f).padding(vertical = 10.dp))
                                TextButton(enabled = !busy, onClick = { selected = entry }) { Text("Actions") }
                            }
                        }
                        if (next.isNotEmpty()) item {
                            TextButton(enabled = !busy, onClick = {
                                work {
                                    val page = app.drive.list(checkNotNull(folder).entryId, next)
                                    entries = (entries + page.entriesList).distinctBy(Entry::getEntryId)
                                    next = page.nextPageToken
                                }
                            }) { Text("Load more") }
                        }
                    }
                }
            }
        }

        if (confirmingDiscard && pendingUpload != null) {
            AlertDialog(onDismissRequest = { confirmingDiscard = false }, title = { Text("Discard pending upload?") },
                confirmButton = {
                    TextButton(onClick = {
                        confirmingDiscard = false
                        work {
                            val cache = session.cacheScope ?: throw SignInRequired()
                            app.uploads.pending(cache)?.uploadId?.let { app.scopedDrive(cache).cancelUpload(it) }
                            app.uploads.discard(cache)
                            pendingUpload = null
                        }
                    }) { Text("Discard") }
                }, dismissButton = { TextButton(onClick = { confirmingDiscard = false }) { Text("Keep") } })
        }

        selected?.let { entry ->
            AlertDialog(onDismissRequest = { selected = null }, title = { Text(entry.name) },
                text = {
                    Column {
                        if (entry.kind == EntryKind.ENTRY_KIND_FILE) {
                            if (supportsTextPreview(entry)) {
                                TextButton(onClick = {
                                    selected = null
                                    work { preview = entry.name to app.drive.previewText(entry) }
                                }) { Text("Preview text") }
                            }
                            TextButton(onClick = {
                                selected = null
                                pendingDownload = Base64.encodeToString(entry.toByteArray(), Base64.NO_WRAP)
                                save.launch(entry.name)
                            }) { Text("Save a copy") }
                        }
                        TextButton(onClick = { rename = entry; selected = null }) { Text("Rename") }
                        TextButton(onClick = { confirmingTrash = true }) { Text("Move to trash") }
                    }
                }, confirmButton = { TextButton(onClick = { selected = null }) { Text("Close") } })
            if (confirmingTrash) AlertDialog(onDismissRequest = { confirmingTrash = false },
                title = { Text("Move ${entry.name} to trash?") },
                confirmButton = {
                    TextButton(onClick = {
                        confirmingTrash = false
                        selected = null
                        work { app.drive.trash(entry); load(folderId) }
                    }) { Text("Move to trash") }
                }, dismissButton = { TextButton(onClick = { confirmingTrash = false }) { Text("Cancel") } })
        }
        if (ready) preview?.let { (filename, contents) ->
            AlertDialog(
                onDismissRequest = { preview = null },
                title = { Text(filename) },
                text = {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (contents.truncated) Text("Showing the first 32 KiB.")
                        SelectionContainer {
                            Text(
                                contents.text.ifEmpty { "Empty file" },
                                modifier = Modifier.heightIn(max = 360.dp).verticalScroll(rememberScrollState()),
                                style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                            )
                        }
                    }
                },
                confirmButton = { TextButton(onClick = { preview = null }) { Text("Close") } },
            )
        }
        if (creating || rename != null) {
            val target = rename
            var name by remember(target?.entryId) { mutableStateOf(target?.name.orEmpty()) }
            fun dismiss() { creating = false; rename = null }
            AlertDialog(onDismissRequest = ::dismiss,
                title = { Text(if (target == null) "New folder" else "Rename") },
                text = { OutlinedTextField(name, { name = it }, label = { Text("Name") }, singleLine = true) },
                confirmButton = {
                    TextButton(enabled = name.isNotBlank(), onClick = {
                        dismiss()
                        work {
                            val parent = checkNotNull(folder)
                            if (target == null) app.drive.createFolder(parent, name)
                            else app.drive.rename(target, parent, name)
                            load(folderId)
                        }
                    }) { Text("Save") }
                }, dismissButton = { TextButton(onClick = ::dismiss) { Text("Cancel") } })
        }
    }

    private suspend fun removeIncomplete(uri: Uri) {
        withContext(Dispatchers.IO + NonCancellable) {
            runCatching { DocumentsContract.deleteDocument(contentResolver, uri) }
        }
    }
}

private class UploadDestinationChanged : IOException()
