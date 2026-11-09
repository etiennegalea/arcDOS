# File manager

Enable the Linux file API and open `/files/` on the same server. Connect with its development token. The page keeps that token in memory and asks for it again after a reload. Use a private instance with synthetic data during development. [File API setup](deployment/README.md#enable-the-linux-file-api)

## Browse and organize

- Open folders from **My files**. Use breadcrumbs and browser back and forward to revisit folders.
- Switch between list and grid. Filtering and sort order apply to the loaded pages. Scroll toward the end or choose **Load more** to continue the directory listing.
- Use the checkboxes to select multiple items. Shift-selection includes the entries between the current item and the previous selection.
- Choose **Copy** for regular files or **Cut** for files and folders, open the destination, and choose **Paste**. You can also drag selected entries onto a folder. Moves and copies preserve occupied destinations.
- Use **New folder**, **Rename**, and **Trash** for changes. Open **Trash**, select an entry, and choose **Restore** to return it to its original location.
- Star files and folders to see them under **Favorites**. **Recent** tracks files opened, downloaded, or uploaded from this browser. Both views keep opaque IDs in browser preferences.
- The workspace checks the server change journal while the page is visible and offers a refresh when it detects a change.

| Shortcut | Action |
| --- | --- |
| Ctrl or Command + K | Focus the filter |
| Arrow keys, Page Up, Page Down, Home, End | Move focus within file entries |
| Enter | Open the focused entry |
| Space | Toggle selection for the focused entry |
| Ctrl or Command + A | Select the filtered entries on loaded pages |
| Ctrl or Command + C, X, V | Copy, cut, or paste |
| F2 | Rename a selected entry |
| Delete | Open the trash confirmation |
| Escape | Close a dialog or clear the selection |

## Transfer and preview

Select **Upload** or drop local files onto the workspace or a folder. The transfer panel shows hashing, acknowledged upload bytes, publication, and download progress. Interrupted uploads can retry from the server's committed offset while the page is open. Large downloads stream to a file picker on supported browsers. Other browsers use a bounded 64 MiB download buffer.

Open a regular file to preview raster images, PDF, text and code, or browser-supported audio and video. Text previews show at most the first 128 KiB. Larger images and documents can be downloaded. Grid thumbnails load for visible items and are held in a bounded in-memory cache.

Audio and video stream and seek with HTTP range requests when the browser supports service workers and transferable streams. This requires HTTPS or a trusted loopback origin. The worker relays requests through the open preview and stores no credentials or file bytes. In browsers without those capabilities, the preview uses a bounded in-memory copy.
