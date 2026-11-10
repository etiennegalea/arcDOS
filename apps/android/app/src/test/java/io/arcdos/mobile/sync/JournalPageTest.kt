package io.arcdos.mobile.sync

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class JournalPageTest {
    @Test
    fun keepsLatestEventPerEntryAndAllowsSequenceGaps() {
        val move = JournalEvent(12, "notes", 2)
        val remove = JournalEvent(15, "notes", 8)
        val create = JournalEvent(13, "other", 1)
        val page = JournalPage(listOf(move, create, remove), 15)

        assertEquals(setOf(remove, create), page.latestChanges(9, 100).toSet())
        assertEquals(emptyList<JournalEvent>(), JournalPage(emptyList(), 15).latestChanges(15, 100).toList())
    }

    @Test
    fun rejectsOversizedUnorderedAndMismatchedPages() {
        val first = JournalEvent(10, "first", 1)
        val second = JournalEvent(11, "second", 8)

        assertThrows(IllegalArgumentException::class.java) {
            JournalPage(listOf(first, second), 11).latestChanges(9, 1)
        }
        assertThrows(IllegalArgumentException::class.java) {
            JournalPage(listOf(second, first), 11).latestChanges(9, 100)
        }
        assertThrows(IllegalArgumentException::class.java) {
            JournalPage(listOf(first), 11).latestChanges(9, 100)
        }
        assertThrows(IllegalArgumentException::class.java) {
            JournalPage(emptyList(), 8).latestChanges(9, 100)
        }
    }
}
