(define-module (activity)
  #:use-module (ice-9 match)
  #:use-module (ice-9 popen)
  #:use-module (ice-9 textual-ports)
  #:use-module (json)
  #:use-module (rnrs bytevectors)
  #:use-module (srfi srfi-1)
  #:export (hearth-activity))

(define query
  '(letrec ((capture
              (lambda (thunk)
                (catch #t
                  (lambda () (list (cons 'ok #t) (cons 'value (thunk))))
                  (lambda arguments
                    (list (cons 'ok #f) (cons 'error (object->string arguments))))))))
     (let ((watches (capture (lambda () (watch-list)))))
       (list
         (cons 'generation (assq-ref (workspace-state) 'generation))
         (cons 'sessions
           (capture (lambda ()
                      (session-list #:fields '(id provider status tty delivery)))))
         (cons 'emacs-sessions
           (capture (lambda ()
                      (map (lambda (row)
                             (list (cons 'id (assq-ref row 'id))
                                   (cons 'buffer (assq-ref row 'buffer))))
                           (tool-call 'emacs-sessions '())))))
         (cons 'layout (capture (lambda () (tool-call 'emacs-layout '()))))
         (cons 'watches watches)
         (cons 'notices
           (if (assq-ref watches 'ok)
               (capture
                 (lambda ()
                   (let loop ((remaining (assq-ref watches 'value)) (seen '()) (result '()))
                     (if (null? remaining) result
                         (let ((subscriber (caar remaining)))
                           (if (memq subscriber seen)
                               (loop (cdr remaining) seen result)
                               (loop (cdr remaining) (cons subscriber seen)
                                     (append result (notice-inbox subscriber 0)))))))))
               '((ok . #f) (error . "Watch discovery unavailable"))))))))

(define (command-output . arguments)
  (let ((port (apply open-pipe* OPEN_READ "timeout" "10" "weft" arguments))
        (status #f)
        (text #f))
    (dynamic-wind
      (lambda () #t)
      (lambda () (set! text (get-string-n port 1048577)))
      (lambda () (set! status (close-pipe port))))
    (unless (zero? status) (error "Weft activity export failed" status text))
    (unless (and (string? text)
                 (<= (bytevector-length (string->utf8 text)) 1048576))
      (error "Weft activity export exceeds 1 MiB"))
    text))

(define (read-value text)
  (call-with-input-string text
    (lambda (port)
      (let ((value (read port)))
        (unless (eof-object? (read port)) (error "Trailing Weft activity data"))
        value))))

(define (isolated-generation)
  (let ((reply (read-value (command-output "workspace-eval" "(workspace-state)"))))
    (unless (eq? 'ok (assq-ref reply 'status))
      (error "Cannot inspect isolated workspace generation" reply))
    (let ((generation (assq-ref (read-value (assq-ref reply 'value)) 'generation)))
      (unless (string? generation) (error "Missing isolated generation"))
      generation)))

(define (atom-text value)
  (cond ((string? value) value)
        ((symbol? value) (symbol->string value))
        (else (object->string value))))

(define (nullable-text value)
  (if (or (not value) (eq? value 'null)) 'null (atom-text value)))

(define (hearth-activity)
  "Export observed Weft activity; missing enrichment remains null with warnings."
  (let* ((reply (read-value (command-output "eval" (object->string query))))
         (raw (and (eq? 'ok (assq-ref reply 'status))
                   (not (assq-ref reply 'truncated))
                   (read-value (assq-ref reply 'value))))
         (warnings '()))
    (unless (list? raw) (error "Cannot inspect trusted Weft activity" reply))
    (define (warn! text)
      (set! warnings (cons (substring text 0 (min 4096 (string-length text))) warnings)))
    (define (source name fallback)
      (let ((entry (assq-ref raw name)))
        (if (and entry (assq-ref entry 'ok))
            (assq-ref entry 'value)
            (begin
              (warn! (format #f "~a unavailable: ~a" name
                             (and entry (assq-ref entry 'error))))
              fallback))))
    (define (bounded name values count)
      (when (> (length values) count)
        (warn! (format #f "~a truncated to ~a entries" name count)))
      (take values (min count (length values))))
    (let* ((session-result (source 'sessions '((sessions) (warnings))))
           (sessions (assq-ref session-result 'sessions))
           (hosts (source 'emacs-sessions '()))
           (layout (source 'layout #f))
           (buffers (if layout
                        (vector->list (assoc-ref (json-string->scm layout) "buffers"))
                        '()))
           (watches (source 'watches '()))
           (notices (source 'notices '()))
           (owner-state
             (catch #t
               (lambda ()
                 (let* ((generation (isolated-generation))
                        (effects (vector->list
                                   (assoc-ref (json-string->scm (command-output "owner" "view")) "effects"))))
                   (unless (equal? generation (isolated-generation))
                     (error "Isolated workspace restarted during owner export"))
                   (cons generation effects)))
               (lambda arguments
                 (warn! (format #f "Owner effects unavailable: ~s" arguments))
                 #f)))
           (effects (if owner-state (cdr owner-state) '())))
      (for-each (lambda (warning) (warn! (object->string warning)))
                (assq-ref session-result 'warnings))
      (define (session-json session)
        (let* ((id (assq-ref session 'id))
               (enriched (find (lambda (row) (equal? id (assq-ref row 'id))) hosts))
               (name (and enriched (assq-ref enriched 'buffer)))
               (buffer (and name (find (lambda (row) (equal? name (assoc-ref row "name"))) buffers))))
          `((id . ,id) (provider . ,(atom-text (assq-ref session 'provider)))
            (status . ,(atom-text (assq-ref session 'status)))
            (delivery . ,(nullable-text (assq-ref session 'delivery)))
            (host . ,(if buffer
                         `((buffer . ,name) (mode . ,(assoc-ref buffer "mode"))
                           (tty . ,(nullable-text (assoc-ref buffer "tty"))))
                         'null)))))
      (define (watch-json watch)
        (match watch
          ((subscriber (kind value) options ...)
           `((subscriber . ,(atom-text subscriber))
             (target . ((kind . ,(atom-text kind)) (value . ,(atom-text value))))
             (delivery . ,(nullable-text (assq-ref options 'deliver)))))
          (_ (error "Unexpected Weft watch" watch))))
      (define (notice-json notice)
        `((id . ,(assq-ref notice 'id))
          (subscriber . ,(atom-text (assq-ref notice 'subscriber)))
          (kind . ,(atom-text (assq-ref notice 'kind)))
          (source . ,(atom-text (assq-ref notice 'source)))
          (text . ,(assq-ref notice 'text))))
      (let ((result
              `((generation . ,(assq-ref raw 'generation))
                (sessions . ,(list->vector (map session-json (bounded 'sessions sessions 512))))
                (watches . ,(list->vector (map watch-json (bounded 'watches watches 128))))
                (notices . ,(list->vector
                              (map notice-json
                                   (bounded 'notices
                                     (sort notices (lambda (a b) (< (assq-ref a 'id) (assq-ref b 'id)))) 128))))
                (leases . ,(list->vector
                             (filter-map
                               (lambda (buffer)
                                 (let ((holder (assoc-ref buffer "holder")))
                                   (and (string? holder)
                                        `((buffer . ,(assoc-ref buffer "name"))
                                          (holder . ,holder) (expires_at_ms . null))))) buffers)))
                (effects . ,(list->vector (bounded 'effects effects 256)))
                (effects_generation . ,(if owner-state (car owner-state) 'null))
                (notice_history_limit . 128)
                (notice_scope . "current-watch-subscribers"))))
        (append result `((warnings . ,(list->vector (bounded 'warnings (reverse warnings) 64)))))))))

(when (string-suffix? "/activity.scm" (car (command-line)))
  (match (cdr (command-line))
    ((or () ("--once"))
     (catch #t
       (lambda ()
         (let ((json (scm->json-string (hearth-activity))))
           (when (> (bytevector-length (string->utf8 json)) 1048576)
             (error "Activity snapshot exceeds 1 MiB"))
           (display json) (newline)))
       (lambda (key . arguments)
         (format (current-error-port) "Hearth activity: ~s ~s~%" key arguments)
         (exit 1))))
    (_ (display "Usage: guile -L scheme scheme/activity.scm [--once]\n" (current-error-port))
       (exit 2))))
