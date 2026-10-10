class Rect:
    def __init__(self, w, h):
        self.w = w
        self.h = h

    def area(self):
        return self.w * self.h


def scale(rect, factor):
    scaled = Rect(rect.w * factor, rect.h * factor)
    return scaled
