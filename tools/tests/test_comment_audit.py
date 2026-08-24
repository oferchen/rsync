"""Unit tests for the comment-policy audit.

Every case here is a false-positive class that a line-based or unbounded
classifier actually produced when this tool was first written. They are
regression pins on the *classifier*, not on any particular comment in the tree.
"""

from __future__ import annotations

import unittest

from tools.comment_audit import Block, blocks_of, classify, words


def lines_of(source: str) -> list[str]:
    return source.strip("\n").splitlines()


def only_finding(source: str) -> str | None:
    """Classify a snippet whose FIRST comment block is the subject."""
    lines = lines_of(source)
    blocks = blocks_of(lines)
    assert blocks, "snippet has no comment block"
    return classify(blocks[0], lines)


class BlockGroupingTests(unittest.TestCase):
    def test_consecutive_same_marker_lines_form_one_block(self) -> None:
        blocks = blocks_of(lines_of("""
        // first line
        // second line
        let x = 1;
        // a separate block
        """))
        self.assertEqual([len(b.bodies) for b in blocks], [2, 1])

    def test_a_marker_change_starts_a_new_block(self) -> None:
        blocks = blocks_of(lines_of("""
        //! module doc
        /// item doc
        fn f() {}
        """))
        self.assertEqual([b.marker for b in blocks], ["//!", "///"])


class ContinuationLineTests(unittest.TestCase):
    """A wrapped sentence must be judged whole, never line by line."""

    def test_a_continuation_starting_with_temp_is_not_a_debug_marker(self) -> None:
        # Line-based classification read "TEMPlate" as a `TEMP` marker.
        self.assertIsNone(only_finding("""
        // The daemon expands the configured
        // template with no specifier, so nothing is substituted.
        fn expand() {}
        """))

    def test_a_continuation_fragment_is_not_a_restatement(self) -> None:
        # "receiver config." alone looks like a bare restatement; in context it
        # is the tail of a sentence.
        self.assertIsNone(only_finding("""
        /// Builds the argument vector handed to the remote
        /// receiver config.
        fn build_receiver_config() {}
        """))


class ProtectionTests(unittest.TestCase):
    """One upstream reference protects the whole block, including quoted C."""

    def test_quoted_c_under_an_upstream_attribution_is_protected(self) -> None:
        lines = lines_of("""
        // upstream: clientname.c:55-84 - the env-var fallback chain.
        // if ((p = strchr(ipaddr_buf, ' ')) != NULL) *p = '\\0';
        // if (valid_ipaddr(ipaddr_buf, True)) return ipaddr_buf;
        fn peer_address() {}
        """)
        block = blocks_of(lines)[0]
        self.assertRegex(block.text, r"(?i)upstream")

    def test_quoted_c_without_attribution_is_still_reported(self) -> None:
        # The tool cannot know a bare C quote is a citation; reporting it is
        # how the missing attribution gets noticed.
        self.assertEqual(
            only_finding("""
        // if (getpeername(fd, (struct sockaddr *) ss, ss_len)) {
        // }
        fn peer_address() {}
        """),
            "commented-out-code",
        )


class WordBoundaryTests(unittest.TestCase):
    """Keyword detectors must not fire on identifiers that merely start alike."""

    def test_format_number_is_not_commented_out_code(self) -> None:
        self.assertIsNone(only_finding("""
        // format_number tests
        fn t() {}
        """))

    def test_use_chroot_prose_is_not_commented_out_code(self) -> None:
        self.assertIsNone(only_finding("""
        // use_chroot defaults to true
        fn t() {}
        """))

    def test_debug_log_prose_is_not_a_debug_marker(self) -> None:
        self.assertIsNone(only_finding("""
        // debug.log should be excluded
        fn t() {}
        """))


class RustdocTests(unittest.TestCase):
    """Doc comments legitimately contain code and tables."""

    def test_a_fenced_example_is_not_commented_out_code(self) -> None:
        self.assertIsNone(only_finding("""
        /// Copies a file.
        ///
        /// ```
        /// use engine::copy;
        /// let n = copy("a", "b")?;
        /// ```
        fn copy() {}
        """))

    def test_a_markdown_table_rule_is_not_a_banner(self) -> None:
        self.assertIsNone(only_finding("""
        /// | option | effect |
        /// |--------|--------|
        /// | `-a`   | archive |
        fn options() {}
        """))


class PlaceholderTests(unittest.TestCase):
    def test_a_marker_used_as_a_marker_is_reported(self) -> None:
        self.assertEqual(
            only_finding("""
        // TODO: wire this up
        fn t() {}
        """),
            "placeholder",
        )

    def test_prose_about_the_upstream_todo_wire_state_is_not(self) -> None:
        self.assertIsNone(only_finding("""
        // The receiver marks every abbreviated entry TODO so the sender is
        // asked for the current value.
        fn diff() {}
        """))


class RestatementTests(unittest.TestCase):
    def test_a_short_echo_of_the_next_line_is_reported(self) -> None:
        self.assertEqual(
            only_finding("""
        // Reset
        state.reset();
        """),
            "restatement",
        )

    def test_a_comment_adding_a_word_the_code_lacks_is_kept(self) -> None:
        self.assertIsNone(only_finding("""
        // Negotiated version is min(client, server)
        let version = negotiate(a, b);
        """))


class WordsTests(unittest.TestCase):
    def test_snake_and_camel_case_are_split(self) -> None:
        self.assertEqual(words("buffer_sizeHint"), {"buffer", "size", "hint"})

    def test_stopwords_are_dropped(self) -> None:
        self.assertEqual(words("the size of the buffer"), {"size", "buffer"})


if __name__ == "__main__":
    unittest.main()
