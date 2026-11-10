package io.arcdos.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.net.URI
import java.net.URLEncoder
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlin.coroutines.resume
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import net.openid.appauth.AppAuthConfiguration
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationException
import net.openid.appauth.AuthorizationRequest
import net.openid.appauth.AuthorizationResponse
import net.openid.appauth.AuthorizationService
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.ResponseTypeValues
import net.openid.appauth.TokenRequest
import net.openid.appauth.TokenResponse
import net.openid.appauth.connectivity.ConnectionBuilder
import net.openid.appauth.connectivity.DefaultConnectionBuilder
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

/** Keep one instance in the application so concurrent callers share the refresh lock. */
class ArcSession(
    context: Context,
    apiOrigin: String,
    private val issuer: String,
    private val clientId: String,
    private val redirectUri: Uri,
) : AutoCloseable {
    private val issuerUri = Uri.parse(issuer)
    private val issuerOrigin = httpsOrigin(issuerUri)
    private val binding = JSONArray(listOf(apiOrigin, issuer, clientId, redirectUri.toString()))
        .toString().encodeToByteArray()
    private val files = SessionFiles(context.noBackupFilesDir, binding)
    private val connections = ConnectionBuilder { uri ->
        if (httpsOrigin(uri) != issuerOrigin) throw IOException("Unexpected Authentik endpoint.")
        DefaultConnectionBuilder.INSTANCE.openConnection(uri).apply { instanceFollowRedirects = false }
    }
    private val service = lazy {
        val configuration = AppAuthConfiguration.Builder().setConnectionBuilder(connections).build()
        AuthorizationService(context.applicationContext, configuration)
    }
    private val mutex = Mutex()

    private data class SessionRecord(val state: AuthState, val cacheScope: String) {
        fun serialize() = JSONObject()
            .put("state", state.jsonSerialize())
            .put("cacheScope", cacheScope)
    }

    @Volatile private var persisted = files.hasSession()
    @Volatile private var record: SessionRecord? = null

    val signedIn: Boolean get() = persisted
    val locked: Boolean get() = persisted && record == null
    val cacheScope: String? get() = record?.cacheScope

    init {
        val apiUri = Uri.parse(apiOrigin)
        require(httpsOrigin(apiUri) != null && apiUri.path.orEmpty() in listOf("", "/")) {
            "ArcDOS API origin must be HTTPS"
        }
        require(issuerOrigin != null && clientId.isNotBlank()) {
            "Configure an HTTPS Authentik issuer and public client ID"
        }
    }

    suspend fun authorizationIntent(): Intent = mutex.withLock {
        val configuration = discover()
        val request = AuthorizationRequest.Builder(configuration, clientId, ResponseTypeValues.CODE, redirectUri)
            .setScope("openid arcdos offline_access")
            .setPrompt("login")
            .build()
        check(request.codeVerifierChallengeMethod == "S256" && request.state != null && request.nonce != null)
        val intent = withContext(Dispatchers.Main.immediate) {
            service.value.getAuthorizationRequestIntent(request)
        }
        withContext(Dispatchers.IO) {
            files.writePending(
                JSONObject().put("request", request.jsonSerialize())
                    .put("expires", System.currentTimeMillis() + AUTHORIZATION_LIFETIME_MS),
            )
        }
        intent
    }

    suspend fun completeAuthorization(result: Intent) = mutex.withLock {
        val pending = withContext(Dispatchers.IO) { files.consumePending() }
            ?: throw IOException("Sign-in expired. Start again.")
        if (pending.getLong("expires") <= System.currentTimeMillis()) {
            throw IOException("Sign-in expired. Start again.")
        }
        val request = AuthorizationRequest.jsonDeserialize(pending.getJSONObject("request"))
        val response = AuthorizationResponse.fromIntent(result)
        val code = response?.authorizationCode
        if (request.state == null || response?.state != request.state || code.isNullOrBlank()) {
            throw IOException("Authentik sign-in was cancelled or rejected.")
        }
        val verified = AuthorizationResponse.Builder(request)
            .setState(request.state)
            .setAuthorizationCode(code)
            .build()
        val (tokens, _) = tokenRequest(verified.createTokenExchangeRequest())
        if (tokens == null || !tokens.hasBearerToken() || tokens.idToken.isNullOrBlank()) {
            throw IOException("Authentik did not issue the required tokens.")
        }
        val state = AuthState(verified, tokens, null)
        persist(SessionRecord(state, UUID.randomUUID().toString()))
    }

    suspend fun accessToken(expectedScope: String? = null): String? = mutex.withLock {
        val active = record ?: return@withLock null
        if (expectedScope != null && active.cacheScope != expectedScope) return@withLock null
        if (!active.state.needsTokenRefresh) return@withLock active.state.accessToken
        if (active.state.refreshToken == null) {
            if ((active.state.accessTokenExpirationTime ?: 0) > System.currentTimeMillis()) {
                return@withLock active.state.accessToken
            }
            clear()
            return@withLock null
        }

        // Finish persisting a rotated refresh token even if the requesting screen closes.
        withContext(NonCancellable) {
            val (tokens, error) = tokenRequest(active.state.createTokenRefreshRequest())
            when {
                tokens != null && tokens.hasBearerToken() -> {
                    val refreshed = AuthState.jsonDeserialize(active.state.jsonSerialize())
                    refreshed.update(tokens, null)
                    persist(active.copy(state = refreshed))
                    refreshed.accessToken
                }
                error == AuthorizationException.TokenRequestErrors.INVALID_GRANT -> {
                    clear()
                    null
                }
                else -> throw IOException("Could not refresh the Authentik session.")
            }
        }
    }

    suspend fun lock() = mutex.withLock {
        record = null
    }

    /** Call only after the app receives a successful biometric or device-credential result. */
    suspend fun unlock(): Boolean = mutex.withLock {
        if (record != null) return@withLock true
        if (!persisted) return@withLock false
        val restored = withContext(Dispatchers.IO) {
            try {
                val json = files.readSession()
                SessionRecord(AuthState.jsonDeserialize(json.getJSONObject("state")), json.getString("cacheScope"))
            } catch (_: GeneralSecurityException) {
                null
            } catch (_: IOException) {
                null
            } catch (_: JSONException) {
                null
            }
        }
        if (restored?.state?.isAuthorized == true) {
            record = restored
            true
        } else {
            clear()
            false
        }
    }

    /** Drop the device session and ask Authentik to revoke the unlocked device's tokens. */
    suspend fun logout() {
        val state = mutex.withLock {
            val active = record?.state
            clear()
            active
        }
        if (state != null) withContext(Dispatchers.IO) { revoke(state) }
    }

    override fun close() {
        if (service.isInitialized()) service.value.dispose()
    }

    private suspend fun persist(value: SessionRecord) = withContext(NonCancellable) {
        withContext(Dispatchers.IO) { files.writeSession(value.serialize()) }
        record = value
        persisted = true
    }

    private suspend fun clear() = withContext(NonCancellable) {
        record = null
        withContext(Dispatchers.IO) { files.clear() }
        persisted = false
    }

    private suspend fun discover(): AuthorizationServiceConfiguration {
        val configuration: AuthorizationServiceConfiguration? = withContext(Dispatchers.Main.immediate) {
            suspendCancellableCoroutine { continuation ->
                AuthorizationServiceConfiguration.fetchFromIssuer(
                    issuerUri,
                    { result, _ -> continuation.resume(result) },
                    connections,
                )
            }
        }
        if (configuration == null || configuration.discoveryDoc?.issuer != issuer ||
            httpsOrigin(configuration.authorizationEndpoint) != issuerOrigin ||
            httpsOrigin(configuration.tokenEndpoint) != issuerOrigin
        ) {
            throw IOException("Authentik discovery did not match the configured issuer.")
        }
        return configuration
    }

    private suspend fun tokenRequest(request: TokenRequest): Pair<TokenResponse?, AuthorizationException?> =
        withContext(Dispatchers.Main.immediate) {
            suspendCancellableCoroutine { continuation ->
                service.value.performTokenRequest(request) { response, error ->
                    continuation.resume(response to error)
                }
            }
        }

    private fun revoke(state: AuthState) {
        val endpoint = state.authorizationServiceConfiguration?.discoveryDoc?.docJson
            ?.optString("revocation_endpoint")?.takeIf(String::isNotBlank)?.let(Uri::parse)
            ?: return
        if (httpsOrigin(endpoint) != issuerOrigin) return
        for ((hint, token) in listOf("refresh_token" to state.refreshToken, "access_token" to state.accessToken)) {
            if (token == null) continue
            try {
                val body = ("client_id=" + URLEncoder.encode(clientId, "UTF-8") +
                    "&token_type_hint=" + hint + "&token=" + URLEncoder.encode(token, "UTF-8"))
                    .encodeToByteArray()
                val connection = connections.openConnection(endpoint)
                try {
                    connection.connectTimeout = 4_000
                    connection.readTimeout = 4_000
                    connection.requestMethod = "POST"
                    connection.doOutput = true
                    connection.setFixedLengthStreamingMode(body.size)
                    connection.setRequestProperty("Content-Type", "application/x-www-form-urlencoded")
                    connection.outputStream.use { it.write(body) }
                    connection.responseCode
                } finally {
                    connection.disconnect()
                }
            } catch (_: IOException) {
                // Device logout is complete even if the issuer is offline.
            }
        }
    }

    private companion object {
        const val AUTHORIZATION_LIFETIME_MS = 10 * 60 * 1000L

        fun TokenResponse.hasBearerToken() =
            tokenType.equals("Bearer", ignoreCase = true) && !accessToken.isNullOrBlank()

        fun httpsOrigin(uri: Uri?): Pair<String, Int>? {
            if (uri == null) return null
            val parsed = runCatching { URI(uri.toString()) }.getOrNull() ?: return null
            val host = parsed.host ?: return null
            if (parsed.scheme != "https" || parsed.rawUserInfo != null ||
                parsed.rawQuery != null || parsed.rawFragment != null
            ) {
                return null
            }
            return host.lowercase() to (if (parsed.port == -1) 443 else parsed.port)
        }
    }
}

/** Authenticated encryption binds both persisted records to this build's connection settings. */
private class SessionFiles(directory: File, private val binding: ByteArray) {
    private val session = AtomicFile(File(directory, "arcdos-session"))
    private val pending = AtomicFile(File(directory, "arcdos-pending-auth"))

    fun hasSession() = session.baseFile.isFile
    fun readSession() = read(session)
    fun writeSession(value: JSONObject) = write(session, value)
    fun writePending(value: JSONObject) = write(pending, value)

    fun consumePending(): JSONObject? = try {
        read(pending)
    } catch (_: FileNotFoundException) {
        null
    } finally {
        pending.delete()
    }

    fun clear() {
        keystore().deleteEntry(KEY_ALIAS)
        pending.delete()
        session.delete()
    }

    private fun read(file: AtomicFile): JSONObject {
        val encrypted = file.readFully()
        if (encrypted.size <= NONCE_BYTES) throw IOException("Incomplete encrypted session.")
        val secret = key(create = false) ?: throw GeneralSecurityException("Missing session key.")
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, secret, GCMParameterSpec(128, encrypted, 0, NONCE_BYTES))
        authenticate(cipher, file)
        return JSONObject(cipher.doFinal(encrypted, NONCE_BYTES, encrypted.size - NONCE_BYTES).decodeToString())
    }

    private fun write(file: AtomicFile, value: JSONObject) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, checkNotNull(key(create = true)))
        check(cipher.iv.size == NONCE_BYTES)
        authenticate(cipher, file)
        val encrypted = cipher.iv + cipher.doFinal(value.toString().encodeToByteArray())
        val output = file.startWrite()
        try {
            output.write(encrypted)
            file.finishWrite(output)
        } catch (error: Exception) {
            file.failWrite(output)
            throw error
        }
    }

    private fun authenticate(cipher: Cipher, file: AtomicFile) {
        cipher.updateAAD(binding)
        cipher.updateAAD(file.baseFile.name.encodeToByteArray())
    }

    private fun key(create: Boolean): SecretKey? {
        val existing = keystore().getKey(KEY_ALIAS, null) as? SecretKey
        if (existing != null || !create) return existing
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build(),
        )
        return generator.generateKey()
    }

    private fun keystore() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private companion object {
        const val KEY_ALIAS = "io.arcdos.mobile.session.v1"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val NONCE_BYTES = 12
    }
}
