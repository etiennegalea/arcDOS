package io.arcdos.mobile.sync

data class JournalEvent(val sequence: Long, val entryId: String, val kind: Int)

data class JournalPage(val changes: List<JournalEvent>, val nextSequence: Long)

fun interface JournalSource {
    suspend fun changes(afterSequence: Long, pageSize: Int): JournalPage
}

internal fun JournalPage.latestChanges(after: Long, pageSize: Int): Collection<JournalEvent> {
    require(changes.size <= pageSize) { "Change page exceeds the requested size" }
    val latest = linkedMapOf<String, JournalEvent>()
    var sequence = after
    for (event in changes) {
        require(event.sequence > sequence) { "Change sequence did not increase" }
        require(event.entryId.length in 1..128 && event.entryId.all { it in '!'..'~' }) {
            "Invalid change entry ID"
        }
        require(event.kind > 0) { "Unspecified change kind" }
        latest[event.entryId] = event
        sequence = event.sequence
    }
    require(nextSequence == sequence) { "Cursor does not match the last journal event" }
    return latest.values
}
