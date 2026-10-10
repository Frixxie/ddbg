import pytest

from shapes import Rect, scale


def test_area():
    assert Rect(2, 3).area() == 6


def test_scale():
    r = scale(Rect(1, 2), 3)
    assert (r.w, r.h) == (3, 6)


@pytest.mark.parametrize("w,h,expected", [(1, 1, 1), (2, 2, 5)])
def test_param(w, h, expected):
    assert Rect(w, h).area() == expected
