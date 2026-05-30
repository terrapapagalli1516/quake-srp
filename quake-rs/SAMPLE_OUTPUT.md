# quaketool — sample session

```
$ quaketool info samples/pak0.pak
PAK  ../samples/pak0.pak  (4 files, dir crc 0x667a, modified)
       1455  maps/demo.bsp
        856  gfx.wad
        196  progs/soldier.mdl
         60  readme.txt
  2567 bytes of file data across 4 entries

$ quaketool map samples/demo.bsp
top-down view of ../samples/demo.bsp  (88 verts, x:[32,992] y:[32,992])
+------------------------------------------------------------------------------+
|                                 #     #    #                                 |
|                        #    #                  #    #                        |
|    #              #                                      #              #    |
|        #      #                                              #      #        |
|                                                                              |
|           ##                                                    ##           |
|        #      #                                              #      #        |
|                                                                              |
|     #             #                                      #             #     |
|                       #                              #                       |
|   #                                                                      #   |
| #                         #                      #                         # |
|                               #              #                               |
|#                                                                            #|
|                                   #      #                                   |
|                                                                              |
|#                                      #                                     #|
|                                   #      #                                   |
|#                                                                            #|
|                               #              #                               |
| #                         #                      #                         # |
|   #                                                                      #   |
|                       #                              #                       |
|     #             #                                      #             #     |
|                                                                              |
|        #      #                                              #      #        |
|           ##                                                    ##           |
|                                                                              |
|        #      #                                              #      #        |
|    #              #                                      #              #    |
|                        #    #                  #    #                        |
|#                                #     #    #                                #|
+------------------------------------------------------------------------------+

$ quaketool mdl samples/soldier.mdl
MDL  ../samples/soldier.mdl  (alias model, version 6)
  skin size    4 x 4
  vertices     3
  triangles    1
  scale        [1.0, 1.0, 1.0]
  scale_origin [0.0, 0.0, 0.0]
  eyeposition  [0.0, 0.0, 2.0]
  bound radius 10
  flags        0x0
  skins        1 (1 single, 0 group)
  frames       1
    [0] single  "frame1"
```
