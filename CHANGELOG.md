# Changelog

## [0.6.0](https://github.com/actuallyatiger/virtual-audio/compare/v0.5.0...v0.6.0) (2026-10-17)


### Features

* **config:** version the config format, with migrations ([9d79531](https://github.com/actuallyatiger/virtual-audio/commit/9d79531d2228ce2280cc111d528d4b4fb32d8b4e))


### Bug Fixes

* **config:** don't lose a newer build's config ([4c9ed37](https://github.com/actuallyatiger/virtual-audio/commit/4c9ed37cba4623d75718c4adbbc38d86c95b640d))

## [0.5.0](https://github.com/actuallyatiger/virtual-audio/compare/v0.4.1...v0.5.0) (2026-10-16)


### Features

* **update:** install the latest release from a test build ([7ca1216](https://github.com/actuallyatiger/virtual-audio/commit/7ca1216ec7986e9bfdef974bc80845ffc0953cb4))

## [0.4.1](https://github.com/actuallyatiger/virtual-audio/compare/v0.4.0...v0.4.1) (2026-10-16)


### Bug Fixes

* **help:** say how to remove the mixer ([6a5268d](https://github.com/actuallyatiger/virtual-audio/commit/6a5268d64ecb7accecf5ad22d78a1e0190bf9ee5))

## [0.4.0](https://github.com/actuallyatiger/virtual-audio/compare/v0.3.0...v0.4.0) (2026-10-16)


### Features

* a phone over Bluetooth as a source ([b3f4d16](https://github.com/actuallyatiger/virtual-audio/commit/b3f4d1611f7b40aaef214d70dea6e1f0b086419b)), closes [#31](https://github.com/actuallyatiger/virtual-audio/issues/31)
* **dsp:** echo cancellation for mics, WebRTC's AEC3 ([55c24f8](https://github.com/actuallyatiger/virtual-audio/commit/55c24f8843436f548874b3feb3f71696cbae0822)), closes [#32](https://github.com/actuallyatiger/virtual-audio/issues/32)
* hear a mic yourself, separately from sending it to others ([bbfb6a9](https://github.com/actuallyatiger/virtual-audio/commit/bbfb6a9e6bc4820d05e9a13b4b9e351241aabf8b))


### Bug Fixes

* **autostart:** make the sign-in task without running schtasks.exe ([6723270](https://github.com/actuallyatiger/virtual-audio/commit/67232702a459d1c0f74beb1c7e07514a360758fb))
* **autostart:** show Start with Windows still on after as Administrator goes off ([766befa](https://github.com/actuallyatiger/virtual-audio/commit/766befafc5340f9ea47e03ce35ae023368013828))
* declare the Windows versions the exe supports ([f493d25](https://github.com/actuallyatiger/virtual-audio/commit/f493d25c9c8f36ea58d8aad5c723d4cd55ba8adf))


### Reverts

* a phone over Bluetooth as a source ([d6e2ef6](https://github.com/actuallyatiger/virtual-audio/commit/d6e2ef6a7dd675b52cbfef893db68c19d733df53))

## [0.3.0](https://github.com/actuallyatiger/virtual-audio/compare/v0.2.1...v0.3.0) (2026-10-14)


### Features

* **autostart:** offer to update the administrator copy when opening another build ([a466333](https://github.com/actuallyatiger/virtual-audio/commit/a46633369d5e19b01a765848ab2f7edad90ee5c9))
* **autostart:** run an installed copy and switch to it straight away ([3defa6a](https://github.com/actuallyatiger/virtual-audio/commit/3defa6a55177dab508d25174a65112cb99eb395b))
* test builds that don't update themselves, and replacing any other build ([d3448e8](https://github.com/actuallyatiger/virtual-audio/commit/d3448e8e2f410f73acf02c8931009da04554091b))


### Bug Fixes

* **dsp:** level speech the way WebRTC's gain control does ([e8f4c1a](https://github.com/actuallyatiger/virtual-audio/commit/e8f4c1a023c54ede65bd8ceef743b211b9ef0474))
* opening another copy now closes the running one ([54695c8](https://github.com/actuallyatiger/virtual-audio/commit/54695c86a9510d42ec1726b0df4b483d189c09b6))

## [0.2.1](https://github.com/actuallyatiger/virtual-audio/compare/v0.2.0...v0.2.1) (2026-10-14)


### Bug Fixes

* **ui:** error toasts show above dialogs; no toast after Check now ([2942174](https://github.com/actuallyatiger/virtual-audio/commit/29421741ad467dd70a351fd0bc112aad2a1c6686))

## [0.2.0](https://github.com/actuallyatiger/virtual-audio/compare/v0.1.0...v0.2.0) (2026-10-14)


### Features

* **ui:** a Settings dialog ([d237585](https://github.com/actuallyatiger/virtual-audio/commit/d2375850038c15d7acceee9ca87d0f71d96bee11))
* **update:** automatic updates ([07bdcf5](https://github.com/actuallyatiger/virtual-audio/commit/07bdcf5e8a3f1bbfbc2ec2aa0830fea0904480b2)), closes [#25](https://github.com/actuallyatiger/virtual-audio/issues/25)


### Bug Fixes

* **ui:** call the ⚙ a card's settings, and other wording ([439921e](https://github.com/actuallyatiger/virtual-audio/commit/439921e08545fc0d33f45cd734c1e5a0d54bcebd))
* **ui:** plainer text, and Help that's easy to scan ([325399f](https://github.com/actuallyatiger/virtual-audio/commit/325399f41038b59396c5fb50fc3c23672e274f9d))

## 0.1.0 (2026-10-14)


### Features

* built-in soundboard and mic noise suppression/gate ([9e93b93](https://github.com/actuallyatiger/virtual-audio/commit/9e93b93c6189ff8179102e713ada5605e8ae8136))
* **gate:** extra strength, and keeping other sources in time ([00ad870](https://github.com/actuallyatiger/virtual-audio/commit/00ad870e3cf1f8af852fa9ea32fb3a339590c3fd))
* log capture lifecycle, panics and exits; add "Open log file" to tray ([be7813a](https://github.com/actuallyatiger/virtual-audio/commit/be7813a4f3f41fa3475c30a1344ac109bc742e23))
* **mic:** automatic level ([0f18a08](https://github.com/actuallyatiger/virtual-audio/commit/0f18a08be99263b946eed81768d816fdba79f9b2)), closes [#20](https://github.com/actuallyatiger/virtual-audio/issues/20)
* **mic:** automatic noise gate, and mic processing in a dialog ([6387e5c](https://github.com/actuallyatiger/virtual-audio/commit/6387e5cb3d8df43186f41dd9f8da559b1e69f3f8))
* mix a mic and per-app audio into a virtual microphone ([fae8d00](https://github.com/actuallyatiger/virtual-audio/commit/fae8d003633f4127ae08048e03826dc820f2e3c9))
* music playlist source ([#7](https://github.com/actuallyatiger/virtual-audio/issues/7)) ([a4422b2](https://github.com/actuallyatiger/virtual-audio/commit/a4422b271e6b5094f51ec072faa239727ab70f42))
* **music:** clear the playlist; track × always shown ([6d64709](https://github.com/actuallyatiger/virtual-audio/commit/6d64709939abdd53eef1d7a9a348011d2724d800))
* **music:** remove tracks from the playlist ([15885c8](https://github.com/actuallyatiger/virtual-audio/commit/15885c84f0d9a0e1eada2e52612107e14e2cb967))
* rename to virtual-audio-mixer ([59751d6](https://github.com/actuallyatiger/virtual-audio/commit/59751d63d0f2e79f07046e9d2f492febcec9585f))
* **soundboard:** even out volumes ([2e73551](https://github.com/actuallyatiger/virtual-audio/commit/2e73551ba0dbc11ef49671bdadd272ca09c54f8e)), closes [#21](https://github.com/actuallyatiger/virtual-audio/issues/21)
* **soundboard:** hear sounds on your own headphones ([a72ed85](https://github.com/actuallyatiger/virtual-audio/commit/a72ed85fc934218c7741d8ee68c4182b4798e2df))
* **soundboard:** reorder sounds ([ef98e1a](https://github.com/actuallyatiger/virtual-audio/commit/ef98e1aac403df77de819785091945d0d62a91d1))
* **soundboard:** trim sounds, with fades and previews ([#6](https://github.com/actuallyatiger/virtual-audio/issues/6)) ([4796564](https://github.com/actuallyatiger/virtual-audio/commit/4796564137d3e28184d8530ab2e89092e4e5e9cb))
* start with Windows as administrator ([#5](https://github.com/actuallyatiger/virtual-audio/issues/5)) ([7718aef](https://github.com/actuallyatiger/virtual-audio/commit/7718aefcdaa4d5bb67506adb577a02470f6379a7))
* **ui:** listen to the mix, type gain values, reorder sources ([21be9b3](https://github.com/actuallyatiger/virtual-audio/commit/21be9b3c53b3429ca2c1f3702f56db35839873a3))
* **ui:** setup check, help, and section info buttons ([da81acd](https://github.com/actuallyatiger/virtual-audio/commit/da81acdf281b3e6499e5e73436393c24a17d01e3))
* **ui:** theme toggle (automatic, light or dark) ([27cc60f](https://github.com/actuallyatiger/virtual-audio/commit/27cc60f8144aa5fc2d424d5bab4eedf61fb6f6ee)), closes [#12](https://github.com/actuallyatiger/virtual-audio/issues/12)
* **ui:** type Hear sounds, sound and Hear music volumes ([885a763](https://github.com/actuallyatiger/virtual-audio/commit/885a763c29f5ee159b765cfe7ec9858822a3f252))
* versioned releases on GitHub, and the version in the app ([9b2a557](https://github.com/actuallyatiger/virtual-audio/commit/9b2a557c82d5aaddf9563ea8c63dab1f5ff651c9)), closes [#14](https://github.com/actuallyatiger/virtual-audio/issues/14)


### Bug Fixes

* **api:** return 400, not 500, for invalid new sources ([7fae5de](https://github.com/actuallyatiger/virtual-audio/commit/7fae5de13cd20b6aad264362f675a93d96d6a404))
* back up an unreadable config; don't rotate logs on a second launch ([40b56aa](https://github.com/actuallyatiger/virtual-audio/commit/40b56aa52c5936888cca3aba04cca95a5ee3bb16))
* **config:** never start with defaults over an unbacked-up config ([6a0b229](https://github.com/actuallyatiger/virtual-audio/commit/6a0b229f5b37bcd00e99173de4205d3192ee42cd))
* **config:** retry failed config saves instead of dropping changes ([7e5be03](https://github.com/actuallyatiger/virtual-audio/commit/7e5be03d72d3efff3bb1880338d86340f741bba0))
* **engine:** keep slow work off the engine lock; reap dead threads; cap input ([5e63c72](https://github.com/actuallyatiger/virtual-audio/commit/5e63c7280f8a5f9c3be7ec547fe09546a8d00523))
* **engine:** serialize the final config save with supervisor saves ([45d1e7f](https://github.com/actuallyatiger/virtual-audio/commit/45d1e7f09e12d16554f4669cc3a356ab69890995))
* **hotkeys:** don't toggle mute when a hotkey's keys are re-recorded ([cedd048](https://github.com/actuallyatiger/virtual-audio/commit/cedd0483e790aa34849c218002ad598e7f1c12b0))
* **listen:** supervise the monitor so Listen recovers and follows devices ([1b09ced](https://github.com/actuallyatiger/virtual-audio/commit/1b09ced767a307a2aa30174da71ec560c32234bb))
* **logging:** keep the previous run's log as log.previous.txt ([225ede4](https://github.com/actuallyatiger/virtual-audio/commit/225ede4025ba157fa320f35226203284b2c1ce49))
* make "default microphone" follow Windows default changes live ([a8d03db](https://github.com/actuallyatiger/virtual-audio/commit/a8d03db53ebaad6419ea6f68f1dad4e505da3f96))
* **mixer:** correct clock drift between sources and the output ([0610c1a](https://github.com/actuallyatiger/virtual-audio/commit/0610c1adbb8a42c2ce18595e9f4a204e985987c6))
* **mixer:** fade out on underrun and back in after re-priming ([4bb632f](https://github.com/actuallyatiger/virtual-audio/commit/4bb632f6929bf89af348c5e27ce4d2a985d8bcc0))
* **mixer:** pre-allocate scratch buffers off the audio thread ([d382e43](https://github.com/actuallyatiger/virtual-audio/commit/d382e4380bf38f92180574ea8138178f462510d3))
* **music:** playback edge cases, ducking, picker and docs from review ([5852275](https://github.com/actuallyatiger/virtual-audio/commit/5852275fa41fdc7460af0c0a4d41f6b4b34830b7))
* **platform:** open URLs with `open` on macOS in demo mode ([3e6842d](https://github.com/actuallyatiger/virtual-audio/commit/3e6842ddf7289c5bbaddb6c765fe916baba3aeaa))
* **platform:** start the elevated task from a copy in Program Files ([2c8bf30](https://github.com/actuallyatiger/virtual-audio/commit/2c8bf30b49a1e94507a6e06203411d673c93ba8f))
* refuse to capture the output cable's own recording end ([e23fcb1](https://github.com/actuallyatiger/virtual-audio/commit/e23fcb1ca7547d25722e68e5123f6421946b518c))
* **server:** forbid framing the UI to stop clickjacking ([241ed65](https://github.com/actuallyatiger/virtual-audio/commit/241ed65dd107c5362a830924952bfe5e94deb416))
* **soundboard:** fade trimmed edges, and trim editor focus and drag ([f670b63](https://github.com/actuallyatiger/virtual-audio/commit/f670b634b1689f202b9d6d807f662e1b83de9456))
* **soundboard:** review fixes for playback, uploads and file handling ([4477358](https://github.com/actuallyatiger/virtual-audio/commit/44773581adaa036435b9d5dbe80da132205cfada))
* **soundboard:** tidier trim editor; Enter no longer closes it ([a969259](https://github.com/actuallyatiger/virtual-audio/commit/a969259696a5169248b6c7d428e4ebb8dda6616e))
* **ui:** checkbox alignment, "as Administrator", clean meter ends ([9ca7e40](https://github.com/actuallyatiger/virtual-audio/commit/9ca7e404190e6244064d0d441445fb8eef425834))
* **ui:** clicking outside a dialog closes it ([4dac6e6](https://github.com/actuallyatiger/virtual-audio/commit/4dac6e65bffa9c82f00b0706863b247d71fcf503))
* **ui:** dB editor input, drag/reorder and Listen fixes from review ([067a2ee](https://github.com/actuallyatiger/virtual-audio/commit/067a2ee8b73418a896b929af61617151616f2455))
* **ui:** drawn icons in text buttons (headphones, plus, stop, play) ([b09f3fc](https://github.com/actuallyatiger/virtual-audio/commit/b09f3fc9ef77de7b479150686b05d1c1bb7a2ce5))
* **ui:** hotkey editing, Add dialog and accessibility fixes from review ([d61e886](https://github.com/actuallyatiger/virtual-audio/commit/d61e886c39726f7c5bd3a30bd03b5bd722fe5124))
* **ui:** keep both of two quick hotkey edits ([84d2730](https://github.com/actuallyatiger/virtual-audio/commit/84d27305de33913bfd24b605a3607b54d749823d))
* **ui:** put each sound's settings cog inside its box ([9036a1c](https://github.com/actuallyatiger/virtual-audio/commit/9036a1c73a2f86f377f5d6e01a1b91d7f69e3aa4))
* **ui:** set aria-* and data-* props as attributes in el() ([ced1bea](https://github.com/actuallyatiger/virtual-audio/commit/ced1beabac592ada69572aea9269f4afc1aaf552))
* **ui:** soundboard upload queue, toast size and keyboard focus ([da340f8](https://github.com/actuallyatiger/virtual-audio/commit/da340f855953424535efc92bf7acd56b2c561c10))
* **ui:** square icon buttons, roomier dropdown arrow, neutral settings gear ([fe32570](https://github.com/actuallyatiger/virtual-audio/commit/fe32570785134ee6681c666876963d7ad405839a))
* **wasapi:** keep loopback activation params alive past a timeout ([f9929d0](https://github.com/actuallyatiger/virtual-audio/commit/f9929d0d0775d35eab518a271591eba63c97abd2))
* **wasapi:** let the completion handler own timed-out activation state ([4125c54](https://github.com/actuallyatiger/virtual-audio/commit/4125c547194e5c8369477b92de95c3a9c570117e))
* **wasapi:** stop freeing the stack-borrowed process loopback params ([c2e24cb](https://github.com/actuallyatiger/virtual-audio/commit/c2e24cb40b28afbbbffa97b52825df9495b511da))
