# Fences

```python
# hidden: backtick fence with an info string
~~~
# hidden: a tilde line does not close a backtick fence
````
## Visible: a longer backtick run closes the fence

~~~
# hidden: tilde fence
~~~~~
## Visible: a longer tilde run closes

   ```
# hidden: fence indented three spaces
```
## Visible after the indented fence

    ```
## Visible: four spaces is not a fence
	```
# hidden: a tab-indented fence opens
```
## Closing hashes ###
#NoSpace is not a heading
####### Seven is not a heading
#
## Unclosed fence follows
```
# hidden to the end
