"""Regex compilation and matching benchmark.

Adapted from https://github.com/python/performance
Exercises re.compile, re.search, re.match, and re.sub
on various patterns against realistic text input.
"""
import re


EMAIL_TEXT = """
From: alice@example.com
To: bob@example.net, charlie@example.org
Subject: Meeting tomorrow

Hi Bob and Charlie,

Just a reminder about our meeting tomorrow at 3pm.
Please bring the quarterly reports.

Best,
Alice
""" * 20

TEXT = """
The quick brown fox jumps over the lazy dog 123 times!
Contact us at support@example.com or call (555) 123-4567.
Visit https://example.com/path?q=search#anchor for more info.
Server 192.168.1.1 responded with 404 Not Found on 2024-01-15.
""" * 5000

PATTERNS = [
    # Email addresses
    r'[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}',
    # URLs
    r'https?://[^\s]+',
    # IP addresses
    r'\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}',
    # Dates (ISO format)
    r'\d{4}-\d{2}-\d{2}',
    # Phone numbers
    r'\(\d{3}\)\s*\d{3}-\d{4}',
    # Words starting with capital letters
    r'\b[A-Z][a-z]+\b',
    # Hex color codes
    r'#[0-9a-fA-F]{6}\b',
]


def benchmark():
    """Benchmark: compile regex patterns and apply them to text."""
    match_count = 0
    substituted_size = 0
    for pattern in PATTERNS:
        # Compile
        pat = re.compile(pattern)
        # Search all matches
        match_count += len(pat.findall(TEXT))
        # Substitution
        substituted_size += len(pat.sub("__MATCH__", TEXT))

    # Email-specific: search and parse
    email_pat = re.compile(r'(\S+)@(\S+)')
    match_count += len(email_pat.findall(EMAIL_TEXT))

    # Multi-line matching
    match_count += len(re.compile(r'^From:', re.MULTILINE).findall(EMAIL_TEXT))

    # Case-insensitive matching
    match_count += len(re.compile(r'the', re.IGNORECASE).findall(TEXT))
    if match_count <= 0 or substituted_size <= 0:
        raise RuntimeError("regex workload produced no output")
    return match_count, substituted_size


if __name__ == "__main__":
    benchmark()
