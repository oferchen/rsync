impl<'a> CopyContext<'a> {
    /// Reports whether this run models the wire `sent`/`received` totals.
    pub(crate) const fn wire_tally_active(&self) -> bool {
        self.wire_tally.is_some()
    }

    /// Adds a file-list entry to the wire model.
    pub(crate) fn tally_flist_entry(&mut self, entry: protocol::flist::FileEntry, readdir_index: u32) {
        if let Some(tally) = self.wire_tally.as_mut() {
            tally.add_entry(entry, readdir_index);
        }
    }

    /// Feeds whole-file literal data to the wire model when `-z` makes its
    /// token sizes depend on the bytes.
    fn tally_whole_file_data(&mut self, data: &[u8]) {
        if let Some(tally) = self.wire_tally.as_mut()
            && tally.needs_literal_data()
        {
            tally.literal_data(data);
        }
    }

    /// Records the itemize-only exchange a completed action costs on the wire.
    ///
    /// upstream: generator.c:587-592 `itemize()` sends an NDX and iflags for
    /// an entry that moved no data when a significant flag is set, or for any
    /// entry under `-vv`/`-ii`; sender.c:584-586 echoes it back. A created
    /// directory, symlink, device or special always carries `ITEM_IS_NEW`.
    fn tally_item(&mut self, record: &LocalCopyRecord) {
        let Some(tally) = self.wire_tally.as_mut() else {
            return;
        };
        let itemized = match record.action() {
            LocalCopyAction::DirectoryCreated
            | LocalCopyAction::SymlinkCopied
            | LocalCopyAction::DeviceCopied
            | LocalCopyAction::FifoCopied => true,
            LocalCopyAction::MetadataReused => {
                record.change_set().has_any_change() || tally.itemize_unchanged()
            }
            LocalCopyAction::EntryDeleted => {
                tally.deleted(record.relative_path(), record.is_directory());
                false
            }
            _ => false,
        };
        if itemized {
            tally.item(record.relative_path());
        }
    }

    /// Folds the modeled totals into the summary once the walk is complete.
    pub(crate) fn finish_wire_tally(&mut self) {
        if let Some(tally) = self.wire_tally.take() {
            let created_root = self
                .summary
                .destination_root_created()
                .then_some(self.destination_root.as_path());
            let summary = &self.summary;
            let deletes = [
                summary.deleted_regular_files(),
                summary.deleted_dirs(),
                summary.deleted_symlinks(),
                summary.deleted_devices(),
                summary.deleted_specials(),
            ];
            let (sent, received) = tally.finish(created_root, deletes);
            self.summary.record_wire_totals(sent, received);
        }
    }
}
