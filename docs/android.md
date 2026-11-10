# Android client

The first Android increment includes browser-based Authentik sign-in, device credential or biometric unlock, paginated browsing, folder creation, rename, trash, bounded UTF-8 previews, and downloads to the system file picker. File mutations use the server's existing revision preconditions.

Choose **Upload file** to stage one document of up to 64 MiB. The app keeps the staged bytes, checksum, and server upload ID in private storage. After an interruption, unlock and choose **Resume** to continue from the server's committed offset. A destination conflict preserves the existing file and the staged upload. Discard the pending upload before choosing another destination.

**Sync changes** stores a bounded set of recent events and an independent durable cursor per device sign-in. Journal sync is read-only. Background transfer scheduling, camera backup, and bidirectional folder synchronization are later M8 work.

## Configure and build

Create an [Authentik Android public provider](deployment/authentik.md#android-public-provider) with the strict redirect URI `io.arcdos.mobile:/oauth2redirect`. Enable Authorization Code and Refresh Token grants on this provider and include the `offline_access` scope mapping. The app uses the system browser, S256 PKCE, and a distinct public client ID. ArcDOS introspects Android access tokens through the confidential web provider and checks current group membership. [AppAuth for Android](https://github.com/openid/AppAuth-Android) · [Authentik federated introspection](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/#cross-provider-token-introspection-and-revocation)

Set both `ARCDOS_ANDROID_ISSUER` and `ARCDOS_ANDROID_CLIENT_ID` in the server container environment. For the production stack, set them in `/etc/arcdos/production.env` and append `-f deploy/compose.android.yml` to the [production Compose command](deployment/production.md#start-and-verify) before recreating the server. Use the exact issuer URL from discovery, including its trailing slash.

Install JDK 21, Android SDK Platform 36, and Build Tools 36.0.0. Set `ANDROID_HOME` to the SDK directory. From the repository root, [generate the Android bindings](development.md#regenerate-protobuf-bindings), then build and run the JVM tests with the pinned Gradle 9.5.0 wrapper.

```sh
apps/android/gradlew -p apps/android :app:testDebugUnitTest :app:assembleDebug \
  -ParcdosApiOrigin=https://drive.example.com \
  -ParcdosIssuer=https://auth.example.com/application/o/arcdos-android/ \
  -ParcdosClientId=replace-with-android-client-id
```

The API origin, issuer, and public client ID are build settings. They can also be set in the local Gradle user properties file. Keep the confidential client secret on the server. A build without the settings displays a configuration message. [AGP compatibility](https://developer.android.com/build/releases/agp-9-3-0-release-notes#compatibility) · [Gradle distribution checksums](https://gradle.org/release-checksums/)

Install `apps/android/app/build/outputs/apk/debug/app-debug.apk` on a device or emulator using `adb install`. Use synthetic files to verify MFA sign-in, folder paging, rename and trash, a preview and saved download, and journal sync across app restarts. Interrupt an upload by switching the device offline, then reconnect and verify that **Resume** publishes the original bytes. Lock and unlock the app with the device prompt. Sign out and verify that opening files requires signing in again.

The app encrypts Authentik state with an Android Keystore key and associates the ciphertext with the build's server, issuer, client ID, and redirect URI. A process restart locks the local session. Signing out removes its credentials and local journal. The browser's Authentik SSO session can be ended separately.
