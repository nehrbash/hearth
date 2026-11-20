(define-module (scene)
  #:use-module (hearth)
  #:export (scene-rect scene-text scene-group scene-clip hearth-scene-snapshot))

(define (scene-rect bounds color)
  `((type . "rect") (bounds . ,(list->vector bounds))
    (color . ,(list->vector color))))

(define* (scene-text text bounds font-size color #:key
                    (family "DejaVu Sans") (weight 400) (overflow 'clip))
  `((type . "text") (text . ,text) (bounds . ,(list->vector bounds))
    (font_size . ,font-size) (color . ,(list->vector color))
    (family . ,family) (weight . ,weight) (overflow . ,(symbol->string overflow))))

(define (scene-group offset children)
  `((type . "group") (offset . ,(list->vector offset))
    (children . ,(list->vector children))))

(define (scene-clip bounds children)
  `((type . "clip") (bounds . ,(list->vector bounds))
    (children . ,(list->vector children))))

(define* (hearth-scene-snapshot nodes generation revision #:key
                              (workspace "isolated") (zones '()))
  (acons 'scene (list->vector nodes)
         (hearth-snapshot zones generation revision #:workspace workspace)))
