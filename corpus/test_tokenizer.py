import pytest
from build_corpus import tokenize_sentences, tokenize_words


def test_sentence_splitting():
    text = "Hello world. How are you? I'm fine."
    sents = tokenize_sentences(text)
    assert len(sents) == 3


def test_contraction_handling():
    words = tokenize_words("I'm don't won't can't")
    assert words == ["i'm", "don't", "won't", "can't"]


def test_cyrillic_tokenization():
    words = tokenize_words("Здравей свят! Как си?")
    assert words == ["здравей", "свят", "как", "си"]


def test_no_abbreviation_split():
    sents = tokenize_sentences("Dr. Smith went to Washington. He arrived.")
    assert len(sents) == 2  # "Dr." should NOT trigger sentence split


def test_numbers_preserved():
    words = tokenize_words("IPv4 has 2^32 addresses")
    assert "ipv4" in words


def test_empty_input():
    assert tokenize_sentences("") == []
    assert tokenize_words("") == []
