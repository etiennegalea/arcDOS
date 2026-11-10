package io.arcdos.mobile.sync

import android.content.Context
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.Index
import androidx.room.PrimaryKey
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Upsert
import androidx.room.withTransaction
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

@Entity(tableName = "journal_cursor")
internal data class CursorRow(@PrimaryKey val scope: String, val sequence: Long)

@Entity(
    tableName = "journal_change",
    primaryKeys = ["scope", "entryId"],
    indices = [Index(value = ["scope", "sequence"])],
)
internal data class ChangeRow(val scope: String, val entryId: String, val sequence: Long, val kind: Int)

@Dao
internal interface JournalDao {
    @Query("SELECT sequence FROM journal_cursor WHERE scope = :scope")
    suspend fun cursor(scope: String): Long?

    @Query("SELECT * FROM journal_change WHERE scope = :scope ORDER BY sequence DESC LIMIT :limit")
    fun recent(scope: String, limit: Int): Flow<List<ChangeRow>>

    @Upsert
    suspend fun saveCursor(cursor: CursorRow)

    @Upsert
    suspend fun saveChanges(changes: List<ChangeRow>)

    @Query(
        """
        DELETE FROM journal_change WHERE scope = :scope AND entryId IN (
            SELECT entryId FROM journal_change WHERE scope = :scope
            ORDER BY sequence DESC LIMIT -1 OFFSET :keep
        )
        """,
    )
    suspend fun prune(scope: String, keep: Int)

    @Query("DELETE FROM journal_change WHERE scope = :scope")
    suspend fun clearChanges(scope: String)

    @Query("DELETE FROM journal_cursor WHERE scope = :scope")
    suspend fun clearCursor(scope: String)

    @Query("DELETE FROM journal_change")
    suspend fun clearAllChanges()

    @Query("DELETE FROM journal_cursor")
    suspend fun clearAllCursors()
}

@Database(entities = [CursorRow::class, ChangeRow::class], version = 1, exportSchema = false)
internal abstract class JournalDatabase : RoomDatabase() {
    abstract fun journal(): JournalDao
}

/** Stores the latest observed journal event per entry and the associated durable cursor. */
class JournalStore private constructor(private val db: JournalDatabase) {
    private val dao = db.journal()
    private val pullLock = Mutex()

    suspend fun cursor(scope: String): Long {
        validateScope(scope)
        return dao.cursor(scope) ?: 0L
    }

    fun recent(scope: String): Flow<List<JournalEvent>> {
        validateScope(scope)
        return dao.recent(scope, MAX_ENTRIES).map { rows ->
            rows.map { JournalEvent(it.sequence, it.entryId, it.kind) }
        }
    }

    suspend fun sync(scope: String, source: JournalSource, maxPages: Int = 4): Int {
        require(maxPages in 1..20) { "Invalid journal page limit" }
        validateScope(scope)
        return pullLock.withLock {
            var after = cursor(scope)
            var applied = 0
            repeat(maxPages) {
                val page = source.changes(after, PAGE_SIZE)
                val latest = page.latestChanges(after, PAGE_SIZE)
                if (latest.isEmpty()) return@withLock applied
                db.withTransaction {
                    dao.saveChanges(latest.map { ChangeRow(scope, it.entryId, it.sequence, it.kind) })
                    dao.prune(scope, MAX_ENTRIES)
                    dao.saveCursor(CursorRow(scope, page.nextSequence))
                }
                after = page.nextSequence
                applied += page.changes.size
                if (page.changes.size < PAGE_SIZE) return@withLock applied
            }
            applied
        }
    }

    suspend fun clear(scope: String) {
        validateScope(scope)
        pullLock.withLock {
            db.withTransaction {
                dao.clearChanges(scope)
                dao.clearCursor(scope)
            }
        }
    }

    suspend fun clearAll() {
        pullLock.withLock {
            db.withTransaction {
                dao.clearAllChanges()
                dao.clearAllCursors()
            }
        }
    }

    private fun validateScope(scope: String) {
        require(scope.isNotBlank() && scope.length <= 256) { "Invalid journal scope" }
    }

    companion object {
        private const val PAGE_SIZE = 100
        private const val MAX_ENTRIES = 4096
        private var instance: JournalStore? = null

        @Synchronized
        fun open(context: Context): JournalStore {
            return instance ?: JournalStore(
                Room.databaseBuilder(context.applicationContext, JournalDatabase::class.java, "journal.db")
                    .build(),
            ).also { instance = it }
        }
    }
}
