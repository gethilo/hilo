package main

// The first exported function: referenced by the test file.
func Used() {}

// The second exported function: referenced by nothing.
func NeverCalled() {}
