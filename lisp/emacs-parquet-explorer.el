;;; emacs-parquet-explorer.el --- GPU-accelerated Parquet file explorer -*- lexical-binding: t; -*-

;; Author: emacs-parquet-explorer
;; Version: 0.1.0
;; Keywords: convenience, tools, data, arrow, parquet
;; Package-Requires: ((emacs "29.1"))

;;; Commentary:
;; An interactive, GPU-accelerated visual data browser for large Parquet files.
;; It utilizes standard egui interfaces compiled to WebAssembly and connects to
;; the generic emacs-egui framework.

;;; Code:

(require 'cl-lib)

(defgroup emacs-parquet-explorer nil
  "Interactive visual explorer for large Parquet files."
  :group 'tools
  :prefix "emacs-parquet-explorer-")

(defcustom emacs-parquet-explorer-use-native-filter t
  "When non-nil, offload search/filter scans to the native sidecar.
Scans run in the multi-threaded `parquet_filter' binary (via
`make-process') instead of on the single-threaded WASM UI.  When nil,
the UI scans in-process."
  :type 'boolean
  :group 'emacs-parquet-explorer)

(cl-defstruct (emacs-parquet-explorer--sidecar
               (:constructor emacs-parquet-explorer--sidecar-make))
  "State for one session's persistent native filter daemon."
  proc        ; the long-lived process, or nil
  filepath    ; parquet file the daemon was started with
  session     ; emacs-egui session plist (for pushing results back)
  (busy nil)  ; t while a request is in flight
  (pending nil)   ; latest queued request plist (latest wins)
  (acc "")    ; partial stdout line accumulator
  out         ; temp file of the in-flight request's indices
  (building nil)  ; t while the one-time cargo build runs
  inflight)   ; token of the in-flight request

(defvar emacs-parquet-explorer--sidecars (make-hash-table :test 'equal)
  "Map of session id -> `emacs-parquet-explorer--sidecar'.")

(eval-and-compile
  (defvar emacs-parquet-explorer--dir
    (file-name-directory (or load-file-name
                             (bound-and-true-p byte-compile-current-file)
                             buffer-file-name
                             default-directory))
    "Directory containing the emacs-parquet-explorer package files.")

  ;; emacs-egui is not published to MELPA and is intentionally NOT declared in
  ;; `Package-Requires': this package vendors it as a git submodule at the
  ;; repository root, which is also required to build the WASM UI (the Rust SDK
  ;; lives there), so the submodule is the single source of truth.  If emacs-egui
  ;; happens to be installed separately it will be on `load-path' -- but
  ;; `featurep' can still be nil at byte-compile time, so we also check
  ;; `locate-library'.  Only when neither is found do we fall back to the bundled
  ;; submodule copy, erroring if even that is absent.
  (unless (or (featurep 'emacs-egui)
              (locate-library "emacs-egui"))
    (let ((egui-dir (expand-file-name "../emacs-egui/lisp/"
                                      emacs-parquet-explorer--dir)))
      (unless (file-exists-p (expand-file-name "emacs-egui.el" egui-dir))
        (error "emacs-parquet-explorer: emacs-egui not found on `load-path' and \
no bundled copy under %s.  Install emacs-egui, or clone submodules with: \
git submodule update --init --recursive" egui-dir))
      (add-to-list 'load-path egui-dir)))
  (require 'emacs-egui))

;; Version gate
(when (version< emacs-egui-version "0.1.0")
  (error "emacs-parquet-explorer requires emacs-egui >= 0.1.0, found %s"
         emacs-egui-version))

;; Register UI directory
(emacs-egui-register-app "emacs-parquet-explorer"
                         (expand-file-name "../ui/"
                                           emacs-parquet-explorer--dir))

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
                     (let ((val (emacs-egui-get-field payload :value))
                           (col (emacs-egui-get-field payload :column))
                           (row (emacs-egui-get-field payload :row)))
                       (when val
                         (kill-new val)
                         (message "Copied cell [%s, %s] to clipboard: %s" row col val)))))
    
    ;; 3. Register callback for asynchronous CSV export
    (emacs-egui-on session "export-csv"
                   (lambda (payload)
                     (let* ((input-path (emacs-egui-get-field payload :filepath))
                            (default-output (and input-path (concat (file-name-sans-extension input-path) ".csv")))
                            (output-path (and input-path (read-file-name "Export CSV to: " nil nil nil (file-name-nondirectory default-output)))))
                       (if (and input-path output-path)
                           (let ((abs-input (expand-file-name input-path))
                                 (abs-output (expand-file-name output-path)))
                             (if (string= abs-input abs-output)
                                 (user-error "Cannot export to the same file! This would overwrite and truncate the source Parquet file.")
                               (emacs-parquet-explorer--run-export abs-input abs-output)))
                         (message "Export cancelled or source file path is invalid.")))))
    
    ;; 3b. Register callback for native multi-threaded search/filtering
    (when emacs-parquet-explorer-use-native-filter
      (emacs-egui-on session "filter-request"
                     (lambda (payload)
                       (emacs-parquet-explorer--run-filter session payload))))

    ;; 4. Open the buffer in active window
    (switch-to-buffer (plist-get session :buffer))

    ;; 5. Wait a split second for WASM initialization and push initial state.
    ;;    `native_filter' tells the UI whether to route scans to the sidecar.
    (run-with-timer 0.6 nil
                    (lambda ()
                      (emacs-egui-send-state
                       session
                       (list :filepath abs-path
                             :native_filter (if emacs-parquet-explorer-use-native-filter
                                                 t :json-false)))))

    session))

(defun emacs-parquet-explorer--run-export (input-path output-path)
  "Asynchronously convert INPUT-PATH to OUTPUT-PATH (CSV).
Uses the native Rust exporter."
  (let* ((expanded-input (expand-file-name input-path))
         (expanded-output (expand-file-name output-path))
         (manifest-path (expand-file-name "ui/Cargo.toml" (expand-file-name "../" emacs-parquet-explorer--dir)))
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

;; ---------------------------------------------------------------------------
;; Native filter sidecar (persistent process)
;; ---------------------------------------------------------------------------

(defun emacs-parquet-explorer--repo-root ()
  "Repository root (the parent of the lisp directory)."
  (expand-file-name "../" emacs-parquet-explorer--dir))

(defun emacs-parquet-explorer--filter-bin ()
  "Absolute path to the built `parquet_filter' binary."
  (expand-file-name "ui/target/release/parquet_filter"
                    (emacs-parquet-explorer--repo-root)))

(defun emacs-parquet-explorer--filter-manifest ()
  "Absolute path to the UI crate manifest."
  (expand-file-name "ui/Cargo.toml" (emacs-parquet-explorer--repo-root)))

(defun emacs-parquet-explorer--send-result (session token &rest kv)
  "Push a filter result for TOKEN (plus KV, e.g. :path/:error) to SESSION."
  (emacs-egui-send-state session (list :filter_result (apply #'list :token token kv))))

(defun emacs-parquet-explorer--sidecar-send (sc req)
  "Write REQ (plist :token :query :filters) to SC's daemon with a fresh out file."
  (let* ((proc (emacs-parquet-explorer--sidecar-proc sc))
         (out (make-temp-file "parquet-filter-" nil ".json"))
         (prev (emacs-parquet-explorer--sidecar-out sc))
         (line (json-encode (list :token (plist-get req :token)
                                  :query (plist-get req :query)
                                  :filters (plist-get req :filters)
                                  :out out))))
    ;; The previous request's file has already been forwarded; drop it.
    (when (and prev (file-exists-p prev)) (ignore-errors (delete-file prev)))
    (setf (emacs-parquet-explorer--sidecar-out sc) out
          (emacs-parquet-explorer--sidecar-busy sc) t
          (emacs-parquet-explorer--sidecar-inflight sc) (plist-get req :token))
    (process-send-string proc (concat line "\n"))))

(defun emacs-parquet-explorer--sidecar-flush (sc)
  "Send SC's pending request if the daemon is idle and live."
  (let ((req (emacs-parquet-explorer--sidecar-pending sc)))
    (when (and req
               (not (emacs-parquet-explorer--sidecar-busy sc))
               (process-live-p (emacs-parquet-explorer--sidecar-proc sc)))
      (setf (emacs-parquet-explorer--sidecar-pending sc) nil)
      (emacs-parquet-explorer--sidecar-send sc req))))

(defun emacs-parquet-explorer--sidecar-handle-line (sc line)
  "Parse one response LINE from SC's daemon and forward it to the UI."
  (let ((line (string-trim line)))
    (unless (string-empty-p line)
      (let ((resp (ignore-errors (json-read-from-string line))))
        (when resp
          (let ((token (cdr (assq 'token resp)))
                (path (cdr (assq 'path resp)))
                (err (cdr (assq 'error resp)))
                (session (emacs-parquet-explorer--sidecar-session sc)))
            (setf (emacs-parquet-explorer--sidecar-busy sc) nil
                  (emacs-parquet-explorer--sidecar-inflight sc) nil)
            (cond
             (path (emacs-parquet-explorer--send-result session token :path path))
             (err (emacs-parquet-explorer--send-result session token :error err)))
            ;; A newer query may have arrived while we were scanning.
            (emacs-parquet-explorer--sidecar-flush sc)))))))

(defun emacs-parquet-explorer--sidecar-process-filter (sc chunk)
  "Accumulate CHUNK from SC's daemon stdout and dispatch complete lines."
  (let ((acc (concat (emacs-parquet-explorer--sidecar-acc sc) chunk)))
    (while (string-match "\n" acc)
      (let ((line (substring acc 0 (match-beginning 0))))
        (setq acc (substring acc (match-end 0)))
        (emacs-parquet-explorer--sidecar-handle-line sc line)))
    (setf (emacs-parquet-explorer--sidecar-acc sc) acc)))

(defun emacs-parquet-explorer--sidecar-start (sc filepath)
  "Spawn the persistent daemon for FILEPATH and flush any pending request."
  (setf (emacs-parquet-explorer--sidecar-filepath sc) filepath
        (emacs-parquet-explorer--sidecar-acc sc) "")
  (let ((proc (make-process
               :name "parquet-filter-daemon"
               :noquery t
               :connection-type 'pipe
               :stderr (get-buffer-create " *Parquet Filter stderr*")
               :command (list (emacs-parquet-explorer--filter-bin) "--serve" filepath)
               :filter (lambda (_p chunk)
                         (emacs-parquet-explorer--sidecar-process-filter sc chunk))
               :sentinel
               (lambda (p _e)
                 (unless (process-live-p p)
                   (let ((token (emacs-parquet-explorer--sidecar-inflight sc)))
                     (setf (emacs-parquet-explorer--sidecar-proc sc) nil
                           (emacs-parquet-explorer--sidecar-busy sc) nil
                           (emacs-parquet-explorer--sidecar-inflight sc) nil)
                     ;; Don't leave the UI spinning if the daemon died mid-scan.
                     (when token
                       (emacs-parquet-explorer--send-result
                        (emacs-parquet-explorer--sidecar-session sc) token
                        :error "sidecar exited"))))))))
    (setf (emacs-parquet-explorer--sidecar-proc sc) proc)
    (emacs-parquet-explorer--sidecar-flush sc)))

(defun emacs-parquet-explorer--sidecar-build-then-start (sc filepath)
  "Build `parquet_filter' asynchronously, then start the daemon for FILEPATH."
  (setf (emacs-parquet-explorer--sidecar-building sc) t)
  (message "emacs-parquet-explorer: building native filter sidecar...")
  (make-process
   :name "parquet-filter-build"
   :buffer (get-buffer-create " *Parquet Filter build*")
   :noquery t
   :command (list "cargo" "build" "--release"
                  "--manifest-path" (emacs-parquet-explorer--filter-manifest)
                  "--bin" "parquet_filter")
   :sentinel
   (lambda (proc event)
     (when (string-match-p "\\(finished\\|exited\\)" event)
       (setf (emacs-parquet-explorer--sidecar-building sc) nil)
       (if (and (= (process-exit-status proc) 0)
                (file-executable-p (emacs-parquet-explorer--filter-bin)))
           (progn
             (message "emacs-parquet-explorer: native filter sidecar ready")
             (emacs-parquet-explorer--sidecar-start sc filepath))
         (let ((req (emacs-parquet-explorer--sidecar-pending sc))
               (session (emacs-parquet-explorer--sidecar-session sc)))
           (when req
             (setf (emacs-parquet-explorer--sidecar-pending sc) nil)
             (emacs-parquet-explorer--send-result
              session (plist-get req :token) :error "sidecar build failed"))))))))

(defun emacs-parquet-explorer--sidecar-ensure (sc filepath)
  "Ensure SC has a live daemon for FILEPATH, building the binary if needed.
Return non-nil when the daemon is live now (otherwise startup is pending)."
  (let ((proc (emacs-parquet-explorer--sidecar-proc sc)))
    (cond
     ((and (process-live-p proc)
           (equal (emacs-parquet-explorer--sidecar-filepath sc) filepath))
      t)
     (t
      (when (process-live-p proc) (delete-process proc))
      (setf (emacs-parquet-explorer--sidecar-proc sc) nil
            (emacs-parquet-explorer--sidecar-busy sc) nil)
      (cond
       ((file-executable-p (emacs-parquet-explorer--filter-bin))
        (emacs-parquet-explorer--sidecar-start sc filepath)
        t)
       (t
        (unless (emacs-parquet-explorer--sidecar-building sc)
          (emacs-parquet-explorer--sidecar-build-then-start sc filepath))
        nil))))))

(defun emacs-parquet-explorer--sidecar-kill (session-id)
  "Kill the sidecar for SESSION-ID and clean up its temp file."
  (let ((sc (gethash session-id emacs-parquet-explorer--sidecars)))
    (when sc
      (when (process-live-p (emacs-parquet-explorer--sidecar-proc sc))
        (delete-process (emacs-parquet-explorer--sidecar-proc sc)))
      (let ((out (emacs-parquet-explorer--sidecar-out sc)))
        (when (and out (file-exists-p out)) (ignore-errors (delete-file out))))
      (remhash session-id emacs-parquet-explorer--sidecars))))

(defun emacs-parquet-explorer--run-filter (session payload)
  "Handle a filter-request PAYLOAD via the persistent native sidecar.
Records the request as the latest pending (latest wins), (re)starts the
daemon as needed, and sends the request when the daemon is idle.  The
daemon parses the Parquet file once and serves subsequent queries from
memory across all cores."
  (let* ((session-id (plist-get session :id))
         (filepath (emacs-egui-get-field payload :filepath))
         (sc (or (gethash session-id emacs-parquet-explorer--sidecars)
                 (let ((new (emacs-parquet-explorer--sidecar-make :session session)))
                   (puthash session-id new emacs-parquet-explorer--sidecars)
                   (let ((buf (plist-get session :buffer)))
                     (when (buffer-live-p buf)
                       (with-current-buffer buf
                         (add-hook 'kill-buffer-hook
                                   (lambda ()
                                     (emacs-parquet-explorer--sidecar-kill session-id))
                                   nil t))))
                   new))))
    (setf (emacs-parquet-explorer--sidecar-session sc) session)
    (if (not (and filepath (file-readable-p filepath)))
        (emacs-parquet-explorer--send-result
         session (emacs-egui-get-field payload :token) :error "source file not readable")
      (setf (emacs-parquet-explorer--sidecar-pending sc)
            (list :token (emacs-egui-get-field payload :token)
                  :query (or (emacs-egui-get-field payload :query) "")
                  :filters (or (emacs-egui-get-field payload :filters) "[]")))
      (when (emacs-parquet-explorer--sidecar-ensure sc filepath)
        (emacs-parquet-explorer--sidecar-flush sc)))))

(provide 'emacs-parquet-explorer)
;;; emacs-parquet-explorer.el ends here
