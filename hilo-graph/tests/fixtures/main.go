package main

import (
	"fmt"
	"example.com/internal/handler"
)

func main() {
	h := handler.New()
	fmt.Println(h.Handle())
}