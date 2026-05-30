;;; emacs-parquet-explorer.el --- GPU-accelerated Parquet file explorer -*- lexical-binding: t; -*-

;; Author: emacs-parquet-explorer
;; Version: 0.1.0
;; Keywords: convenience, tools, data, arrow, parquet
;; Package-Requires: ((emacs "29.1") (emacs-egui "0.1.0"))

;;; Commentary:
;; An interactive, GPU-accelerated visual data browser for large Parquet files.
;; It utilizes standard egui interfaces compiled to WebAssembly and connects to
;; the generic emacs-egui framework.

;;; Code:

(require 'emacs-egui)

(defgroup emacs-parquet-explorer nil
  "Interactive visual explorer for large Parquet files."
  :group 'tools
  :prefix "emacs-parquet-explorer-")

;;;###autoload
(defun emacs-parquet-explorer-open (file)
  "Open FILE in an interactive GPU-accelerated Parquet data viewer buffer."
  (interactive "fOpen Parquet File: ")
  (let* ((abs-path (expand-file-name file))
         (buf-name (format "*Parquet Explorer: %s*" (file-name-nondirectory abs-path)))
         ;; 1. Instantiate the generic framework buffer
         (session (emacs-egui-create-buffer
                   :app-name "emacs-parquet-explorer"
                   :buffer-name buf-name)))
    
    ;; 2. Register callback for interactive cell selection
    (emacs-egui-on session "cell-selected"
                   (lambda (payload)
                     (let* ((val (plist-get payload :value))
                            (col (plist-get payload :column))
                            (row (plist-get payload :row)))
                       (when val
                         (kill-new val)
                         (message "Copied cell [%s, %s] to clipboard: %s" row col val)))))
    
    ;; 3. Open the buffer in active window
    (switch-to-buffer (plist-get session :buffer))
    
    ;; 4. Wait a split second for WASM initialization and push initial state
    (run-with-timer 0.6 nil
                    (lambda ()
                      (emacs-egui-send-state session (list :filepath abs-path))))
    
    session))

(provide 'emacs-parquet-explorer)
;;; emacs-parquet-explorer.el ends here
