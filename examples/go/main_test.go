package main

import "testing"

func TestGreet(t *testing.T) {
	if got := Greet(); got != "Hello, world!" {
		t.Fatalf("expected %q, got %q", "Hello, world!", got)
	}
}
