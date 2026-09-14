use rusqlite::{Connection, Result, params};

use crate::anchorapp::sms_models::{SmsAddress, SmsAttachment, SmsMessage, SmsThread};

/// Initialize all tables. Safe to call on every startup — uses IF NOT EXISTS.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS threads (
            thread_id    INTEGER PRIMARY KEY,
            last_updated INTEGER NOT NULL DEFAULT 0,
            snippet      TEXT
        );

        CREATE TABLE IF NOT EXISTS messages (
            uid          INTEGER PRIMARY KEY,
            thread_id    INTEGER NOT NULL REFERENCES threads(thread_id),
            body         TEXT,
            date         INTEGER NOT NULL,
            event        INTEGER NOT NULL DEFAULT 0,
            message_type INTEGER NOT NULL DEFAULT 0,
            read         INTEGER NOT NULL DEFAULT 0,
            sub_id       INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_messages_thread_date
            ON messages (thread_id, date DESC);

        CREATE TABLE IF NOT EXISTS addresses (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            message_uid INTEGER NOT NULL REFERENCES messages(uid),
            address     TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_addresses_message
            ON addresses (message_uid);

        CREATE TABLE IF NOT EXISTS thread_addresses (
            thread_id    INTEGER NOT NULL REFERENCES threads(thread_id),
            address      TEXT NOT NULL,
            display_name TEXT,
            photo_data   TEXT,
            PRIMARY KEY (thread_id, address)
        );

        CREATE TABLE IF NOT EXISTS attachments (
            part_id           INTEGER PRIMARY KEY,
            message_uid       INTEGER NOT NULL REFERENCES messages(uid),
            mime_type         TEXT NOT NULL,
            unique_identifier TEXT NOT NULL,
            encoded_thumbnail TEXT,
            local_path        TEXT
        );

        CREATE TABLE IF NOT EXISTS contacts (
            uid          TEXT PRIMARY KEY,
            display_name TEXT,
            last_changed INTEGER NOT NULL,
            vcard        TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS contact_phones (
            contact_uid  TEXT NOT NULL REFERENCES contacts(uid),
            phone_number TEXT NOT NULL,
            PRIMARY KEY (contact_uid, phone_number)
        );

        CREATE TABLE IF NOT EXISTS contact_photos (
            contact_uid TEXT PRIMARY KEY REFERENCES contacts(uid),
            local_path  TEXT NOT NULL
        );
    ",
    )
}

/// Insert a batch of messages, skipping any whose uid already exists.
/// Updates the parent thread's last_updated and snippet after each insert.
pub fn insert_messages(conn: &Connection, messages: &[SmsMessage]) -> Result<()> {
    for msg in messages {
        // Ensure the thread row exists before inserting the message (FK constraint).
        conn.execute(
            "INSERT OR IGNORE INTO threads (thread_id, last_updated) VALUES (?1, ?2)",
            params![msg.thread_id, msg.date],
        )?;

        let inserted = conn.execute(
            "INSERT OR IGNORE INTO messages
                (uid, thread_id, body, date, event, message_type, read, sub_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                msg.uid,
                msg.thread_id,
                msg.body,
                msg.date,
                msg.event,
                msg.message_type,
                msg.read as i32,
                msg.sub_id,
            ],
        )?;

        if inserted == 0 {
            // Already in the DB — skip addresses and attachments too.
            continue;
        }

        for addr in &msg.addresses {
            conn.execute(
                "INSERT INTO addresses (message_uid, address) VALUES (?1, ?2)",
                params![msg.uid, addr.address],
            )?;

            // Keep the thread-level address list up to date, updating name/photo if we get one.
            conn.execute(
                "INSERT INTO thread_addresses (thread_id, address, display_name, photo_data)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(thread_id, address) DO UPDATE SET
                     display_name = COALESCE(?3, display_name),
                     photo_data   = COALESCE(?4, photo_data)",
                params![msg.thread_id, addr.address, addr.name, addr.photo],
            )?;
        }

        for att in &msg.attachments {
            conn.execute(
                "INSERT OR IGNORE INTO attachments
                    (part_id, message_uid, mime_type, unique_identifier, encoded_thumbnail, local_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    att.part_id,
                    msg.uid,
                    att.mime_type,
                    att.unique_identifier,
                    att.encoded_thumbnail,
                    att.local_path,
                ],
            )?;
        }

        // Update thread metadata if this message is newer than what we have.
        conn.execute(
            "UPDATE threads
             SET last_updated = MAX(last_updated, ?1),
                 snippet      = CASE WHEN ?1 >= last_updated THEN ?2 ELSE snippet END
             WHERE thread_id = ?3",
            params![msg.date, msg.body, msg.thread_id],
        )?;
    }

    Ok(())
}

/// Return all threads sorted by most-recent message first.
pub fn get_threads(conn: &Connection) -> Result<Vec<SmsThread>> {
    // Concatenate address, name, and photo as "address\x1fname\x1cphoto" triples separated by \x1e
    let mut stmt = conn.prepare(
        "SELECT t.thread_id, t.last_updated, t.snippet,
                GROUP_CONCAT(ta.address || char(31) || COALESCE(ta.display_name, '') || char(28) || COALESCE(ta.photo_data, ''), char(30)) AS addr_triples
         FROM threads t
         LEFT JOIN thread_addresses ta ON ta.thread_id = t.thread_id
         GROUP BY t.thread_id
         ORDER BY t.last_updated DESC",
    )?;

    let threads = stmt
        .query_map([], |row| {
            let thread_id: i64 = row.get(0)?;
            let last_updated: i64 = row.get(1)?;
            let snippet: Option<String> = row.get(2)?;
            let addr_triples: Option<String> = row.get(3)?;

            let addresses = addr_triples
                .unwrap_or_default()
                .split('\x1e')
                .filter(|s| !s.is_empty())
                .map(|triple| {
                    let mut parts = triple.splitn(2, '\x1f');
                    let address = parts.next().unwrap_or("").to_string();
                    let rest = parts.next().unwrap_or("");
                    let mut name_photo = rest.splitn(2, '\x1c');
                    let name_raw = name_photo.next().unwrap_or("");
                    let photo_raw = name_photo.next().unwrap_or("");
                    let name = if name_raw.is_empty() { None } else { Some(name_raw.to_string()) };
                    let photo =
                        if photo_raw.is_empty() { None } else { Some(photo_raw.to_string()) };
                    SmsAddress { address, name, photo }
                })
                .collect();

            Ok(SmsThread { thread_id, last_updated, addresses, snippet })
        })?
        .collect::<Result<Vec<_>>>()?;

    Ok(threads)
}

/// Return messages for a thread, ordered oldest → newest.
/// Secondary sort by `uid` makes the order deterministic when several
/// messages share the same millisecond timestamp (e.g. rapid sends).
pub fn get_messages(conn: &Connection, thread_id: i64) -> Result<Vec<SmsMessage>> {
    let mut stmt = conn.prepare(
        "SELECT uid, thread_id, body, date, event, message_type, read, sub_id
         FROM messages
         WHERE thread_id = ?1
         ORDER BY date ASC, uid ASC",
    )?;

    let mut messages: Vec<SmsMessage> = stmt
        .query_map(params![thread_id], |row| {
            Ok(SmsMessage {
                uid: row.get(0)?,
                thread_id: row.get(1)?,
                body: row.get(2)?,
                date: row.get(3)?,
                event: row.get(4)?,
                message_type: row.get(5)?,
                read: row.get::<_, i32>(6)? != 0,
                sub_id: row.get(7)?,
                addresses: vec![],
                attachments: vec![],
            })
        })?
        .collect::<Result<Vec<_>>>()?;

    // Populate addresses and attachments per message.
    for msg in &mut messages {
        msg.addresses = get_addresses_for_message(conn, msg.uid)?;
        msg.attachments = get_attachments_for_message(conn, msg.uid)?;
    }

    Ok(messages)
}

/// Return the oldest timestamp we have cached for a thread, or None if the
/// thread has no messages. Used to calculate the fetch range for pagination.
pub fn oldest_message_date(conn: &Connection, thread_id: i64) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT MIN(date) FROM messages WHERE thread_id = ?1",
        params![thread_id],
        |row| row.get(0),
    )
}

/// Mark a downloaded attachment's local path.
pub fn set_attachment_local_path(
    conn: &Connection,
    unique_identifier: &str,
    local_path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE attachments SET local_path = ?1 WHERE unique_identifier = ?2",
        params![local_path, unique_identifier],
    )?;
    Ok(())
}

/// Check whether an attachment file is already cached locally.
pub fn get_attachment_local_path(
    conn: &Connection,
    unique_identifier: &str,
) -> Result<Option<String>> {
    let result = conn.query_row(
        "SELECT local_path FROM attachments WHERE unique_identifier = ?1",
        params![unique_identifier],
        |row| row.get(0),
    );

    match result {
        Ok(path) => Ok(path),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Return downloaded attachment paths before clearing the cache so callers can
/// remove the corresponding files as well as their database records.
pub fn attachment_local_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT local_path FROM attachments WHERE local_path IS NOT NULL AND local_path != ''",
    )?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

/// Delete every locally cached conversation, address, contact, and attachment.
/// The phone remains the source of truth and can repopulate the cache after the
/// user reconnects or opens Messages again.
pub fn clear_all(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         DELETE FROM attachments;
         DELETE FROM addresses;
         DELETE FROM contact_phones;
         DELETE FROM contact_photos;
         DELETE FROM contacts;
         DELETE FROM thread_addresses;
         DELETE FROM messages;
         DELETE FROM threads;
         COMMIT;",
    )
}

fn get_addresses_for_message(conn: &Connection, message_uid: i64) -> Result<Vec<SmsAddress>> {
    let mut stmt = conn.prepare("SELECT address FROM addresses WHERE message_uid = ?1")?;
    stmt.query_map(params![message_uid], |row| {
        Ok(SmsAddress { address: row.get(0)?, name: None, photo: None })
    })?
    .collect()
}

fn get_attachments_for_message(conn: &Connection, message_uid: i64) -> Result<Vec<SmsAttachment>> {
    let mut stmt = conn.prepare(
        "SELECT part_id, mime_type, unique_identifier, encoded_thumbnail, local_path
         FROM attachments WHERE message_uid = ?1",
    )?;
    stmt.query_map(params![message_uid], |row| {
        Ok(SmsAttachment {
            part_id: row.get(0)?,
            mime_type: row.get(1)?,
            unique_identifier: row.get(2)?,
            encoded_thumbnail: row.get(3)?,
            local_path: row.get(4)?,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_all_removes_cached_message_data() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO threads (thread_id, last_updated, snippet) VALUES (1, 1, 'hello')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (uid, thread_id, body, date) VALUES (10, 1, 'hello', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO addresses (message_uid, address) VALUES (10, '+15555550123')",
            [],
        )
        .unwrap();

        clear_all(&conn).unwrap();

        assert!(get_threads(&conn).unwrap().is_empty());
        assert!(get_messages(&conn, 1).unwrap().is_empty());
    }
}
