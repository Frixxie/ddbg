from main import add


def test_adds():
    assert add(2, 3) == 5


def test_fails():
    assert add(2, 2) == 5
