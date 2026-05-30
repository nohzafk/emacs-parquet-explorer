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

(defvar emacs-parquet-explorer--dir
  (file-name-directory (or load-file-name buffer-file-name default-directory))
  "Directory containing the emacs-parquet-explorer package files.")

(defun emacs-parquet-explorer--get-field (payload key)
  "Retrieve KEY from PAYLOAD, supporting both alist and plist formats.
KEY should be a keyword or symbol (e.g. :filepath or 'filepath)."
  (let* ((key-str (symbol-name key))
         (clean-key (if (string-prefix-p ":" key-str)
                        (substring key-str 1)
                      key-str)))
    (if (and payload (listp (car-safe payload)))
        ;; It's an alist!
        (let ((sym (intern clean-key)))
          (cdr (assoc sym payload)))
      ;; It's a plist!
      (let ((kw (intern (concat ":" clean-key))))
        (plist-get payload kw)))))

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
                     (let ((val (emacs-parquet-explorer--get-field payload :value))
                           (col (emacs-parquet-explorer--get-field payload :column))
                           (row (emacs-parquet-explorer--get-field payload :row)))
                       (when val
                         (kill-new val)
                         (message "Copied cell [%s, %s] to clipboard: %s" row col val)))))
    
    ;; 3. Register callback for asynchronous CSV export
    (emacs-egui-on session "export-csv"
                   (lambda (payload)
                     (let* ((input-path (emacs-parquet-explorer--get-field payload :filepath))
                            (default-output (and input-path (concat (file-name-sans-extension input-path) ".csv")))
                            (output-path (and input-path (read-file-name "Export CSV to: " nil nil nil (file-name-nondirectory default-output)))))
                       (if (and input-path output-path)
                           (let ((abs-input (expand-file-name input-path))
                                 (abs-output (expand-file-name output-path)))
                             (if (string= abs-input abs-output)
                                 (user-error "Cannot export to the same file! This would overwrite and truncate the source Parquet file.")
                               (emacs-parquet-explorer--run-export abs-input abs-output)))
                         (message "Export cancelled or source file path is invalid.")))))
    
    ;; 4. Open the buffer in active window
    (switch-to-buffer (plist-get session :buffer))
    
    ;; 5. Wait a split second for WASM initialization and push initial state
    (run-with-timer 0.6 nil
                    (lambda ()
                      (emacs-egui-send-state session (list :filepath abs-path))))
    
    session))

(defun emacs-parquet-explorer--run-export (input-path output-path)
  "Asynchronously convert INPUT-PATH (parquet) to OUTPUT-PATH (csv) using native Rust exporter."
  (let* ((expanded-input (expand-file-name input-path))
         (expanded-output (expand-file-name output-path))
         (manifest-path (expand-file-name "renderer/Cargo.toml" (expand-file-name "../" emacs-parquet-explorer--dir)))
         (buf (get-buffer-create "*Parquet Export*"))
         (proc-name "parquet-export-process"))
    (message "Exporting Parquet to CSV...")
    ;; Clean up previous logs
    (with-current-buffer buf
      (setq-local buffer-read-only nil)
      (erase-buffer))
    (make-process
     :name proc-name
     :buffer buf
     :command (list "cargo" "run" "--release" "--manifest-path" manifest-path "--bin" "parquet_to_csv" "--" expanded-input expanded-output)
     :sentinel (lambda (proc event)
                 (when (string-match-p "\\(finished\\|exited\\)" event)
                   (let ((exit-status (process-exit-status proc)))
                     (if (= exit-status 0)
                         (message "Successfully exported Parquet to %s" expanded-output)
                       (message "Export failed! Check buffer *Parquet Export* for errors.")
                       (display-buffer buf))))))))

(provide 'emacs-parquet-explorer)
;;; emacs-parquet-explorer.el ends here
