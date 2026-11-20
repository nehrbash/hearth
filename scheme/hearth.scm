(define-module (hearth)
  #:use-module (ice-9 match)
  #:use-module (json)
  #:use-module (rnrs bytevectors)
  #:export (hearth-snapshot hearth-owner-snapshot hearth-send!
            hearth-text hearth-metric hearth-table hearth-wiring))

(define (hearth-text name text) (list name 'text text))
(define (hearth-metric name value) (list name 'metric value))
(define (hearth-table name rows) (list name 'table rows))
(define (hearth-wiring name edges) (list name 'wiring edges))

(define (cell-text value)
  (if (string? value) value (object->string value)))

(define (zone-json zone)
  (match zone
    ((name type data)
     (unless (and (symbol? name) (memq type '(text metric table wiring)))
       (error "Expected a named typed Weft zone" zone))
     (let ((rows (case type
                   ((table wiring)
                    (list->vector
                      (map (lambda (row)
                             (when (and (eq? type 'wiring) (not (= 2 (length row))))
                               (error "Wiring edges need two endpoints" row))
                             (list->vector (map cell-text row))) data)))
                   (else #())))
           (display (case type
                      ((text)
                       (unless (string? data) (error "Text zone needs a string"))
                       data)
                      ((metric)
                       (unless (or (string? data) (number? data))
                         (error "Metric zone needs a number or string"))
                       (cell-text data))
                      (else ""))))
       `((name . ,(symbol->string name)) (type . ,(symbol->string type))
         (display . ,display) (rows . ,rows))))
    (_ (error "Expected (name type data)" zone))))

(define (snapshot zones generation revision workspace)
  (unless (and (string? generation) (string? workspace)
               (integer? revision) (exact? revision) (<= 0 revision 18446744073709551615))
    (error "Expected workspace/generation strings and an unsigned 64-bit revision"))
  `((version . 1) (kind . "snapshot") (workspace . ,workspace)
    (generation . ,generation) (revision . ,revision) (zones . ,zones)))

(define* (hearth-snapshot zones generation revision #:key (workspace "isolated"))
  "Build a Hearth snapshot from Weft's zone-list data."
  (snapshot (list->vector (map zone-json zones)) generation revision workspace))

(define* (hearth-owner-snapshot owner-json generation revision #:key (workspace "isolated"))
  "Build a Hearth snapshot from weft owner view JSON, excluding effects."
  (let ((zones (assoc-ref (json-string->scm owner-json) "zones")))
    (unless (vector? zones) (error "Owner view needs a zones array"))
    (snapshot zones generation revision workspace)))

(define (hearth-send! path value)
  "Send one snapshot to Hearth's private socket; return its UTF-8 byte count."
  (let* ((json (scm->json-string value))
         (size (bytevector-length (string->utf8 json))))
    (when (> size 1048576) (error "Hearth snapshot exceeds 1 MiB"))
    (let ((port (socket AF_UNIX SOCK_STREAM 0)))
      (dynamic-wind
        (lambda () #t)
        (lambda ()
          (connect port AF_UNIX path)
          (display json port)
          (newline port)
          (force-output port)
          size)
        (lambda () (close-port port))))))
