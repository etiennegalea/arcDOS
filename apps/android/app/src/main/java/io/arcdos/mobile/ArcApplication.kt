package io.arcdos.mobile

import android.app.Application
import android.net.Uri
import io.arcdos.mobile.sync.JournalStore

internal class ArcApplication : Application() {
    val configured = listOf(BuildConfig.ARCDOS_API_ORIGIN, BuildConfig.ARCDOS_ISSUER,
        BuildConfig.ARCDOS_CLIENT_ID).all(String::isNotEmpty)
    val session by lazy {
        ArcSession(this, BuildConfig.ARCDOS_API_ORIGIN, BuildConfig.ARCDOS_ISSUER,
            BuildConfig.ARCDOS_CLIENT_ID, Uri.parse("io.arcdos.mobile:/oauth2redirect"))
    }
    val drive by lazy { DriveApi(BuildConfig.ARCDOS_API_ORIGIN) { session.accessToken() } }
    val journal by lazy { JournalStore.open(this) }
    val uploads by lazy { UploadQueue(this) }

    fun scopedDrive(scope: String) = DriveApi(BuildConfig.ARCDOS_API_ORIGIN) {
        session.accessToken(scope)
    }
}
