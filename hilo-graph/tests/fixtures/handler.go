package handler

import (
	"net/http"
	"example.com/internal/middleware"
)

type Handler struct{}

func New() *Handler {
	return &Handler{}
}

func (h *Handler) Handle() string {
	mw := middleware.AuthMiddleware(http.DefaultServeMux)
	return mw != nil
}