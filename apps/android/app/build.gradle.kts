import java.net.URI

plugins {
    id("com.android.application") version "9.3.0"
    kotlin("android") version "2.4.21"
    kotlin("kapt") version "2.4.21"
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.21"
}

fun serverSetting(property: String, allowPath: Boolean = false): String {
    val value = providers.gradleProperty(property).orElse("").get()
    if (value.isEmpty()) return "\"\""
    val url = URI.create(value)
    require(url.scheme == "https" && url.host != null && url.userInfo == null &&
        url.query == null && url.fragment == null && (allowPath || url.path.isEmpty()) &&
        value.all { it.isLetterOrDigit() || it in ":/._~%-[]" }) {
        "$property must be an HTTPS URL${if (allowPath) "" else " origin without a trailing slash"}"
    }
    return "\"$value\""
}

val clientId = providers.gradleProperty("arcdosClientId").orElse("").get()
require(clientId.matches(Regex("[A-Za-z0-9_.-]*"))) { "Invalid arcdosClientId" }
val generated = rootProject.file("../../gen/android")

android {
    namespace = "io.arcdos.mobile"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.arcdos.mobile"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        manifestPlaceholders["appAuthRedirectScheme"] = "io.arcdos.mobile"
        buildConfigField("String", "ARCDOS_API_ORIGIN", serverSetting("arcdosApiOrigin"))
        buildConfigField("String", "ARCDOS_ISSUER", serverSetting("arcdosIssuer", allowPath = true))
        buildConfigField("String", "ARCDOS_CLIENT_ID", "\"$clientId\"")
    }

    sourceSets.getByName("main").java.srcDir(generated)
    buildFeatures {
        buildConfig = true
        compose = true
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions.jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
}

tasks.named("preBuild") {
    doFirst {
        check(generated.resolve("io/arcdos/files/v1/FileServiceClient.kt").isFile) {
            "Generate Android bindings with buf generate from the repository root."
        }
    }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2025.11.01"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.12.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.fragment:fragment-ktx:1.8.9")
    implementation("androidx.biometric:biometric:1.1.0")
    implementation("androidx.room:room-runtime:2.8.4")
    implementation("androidx.room:room-ktx:2.8.4")
    kapt("androidx.room:room-compiler:2.8.4")
    // Room's default metadata reader predates the Kotlin compiler used by the app.
    kapt("org.jetbrains.kotlin:kotlin-metadata-jvm:2.4.21")
    implementation("net.openid:appauth:0.11.1")
    implementation("com.connectrpc:connect-kotlin-okhttp:0.10.0")
    implementation("com.connectrpc:connect-kotlin-google-javalite-ext:0.10.0")
    implementation("com.google.protobuf:protobuf-javalite:4.36.2")
    implementation("com.google.protobuf:protobuf-kotlin-lite:4.36.2")
    implementation("com.squareup.okhttp3:okhttp:5.5.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    testImplementation("junit:junit:4.13.2")
}

dependencyLocking {
    lockAllConfigurations()
}
