package handler

import (
	"testing"
)

func TestHandler(t *testing.T) {
	h := New()
	if h == nil {
		t.Fatal("New() returned nil")
	}
}